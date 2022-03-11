use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    future::Future,
    hash::Hash,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use async_std::{
    task::{Builder, JoinHandle},
    task_local,
};
use chashmap::CHashMap;
use event_listener::Event;

use crate::{
    slot::SlotRef, task::NativeTaskFuture, task_input::TaskInput, NativeFunction, Task, TraitType,
};

pub struct TurboTasks {
    resolve_task_cache: CHashMap<(&'static NativeFunction, Vec<TaskInput>), Arc<Task>>,
    native_task_cache: CHashMap<(&'static NativeFunction, Vec<TaskInput>), Arc<Task>>,
    trait_task_cache: CHashMap<(&'static TraitType, String, Vec<TaskInput>), Arc<Task>>,
    currently_scheduled_tasks: AtomicUsize,
    scheduled_tasks: AtomicUsize,
    start: Mutex<Option<Instant>>,
    last_update: Mutex<Option<(Duration, usize)>>,
    event: Event,
}

task_local! {
    static TURBO_TASKS: RefCell<Option<Arc<TurboTasks>>> = RefCell::new(None);
    static TASKS_TO_NOTIFY: Cell<Vec<Arc<Task>>> = Default::default();
}

impl TurboTasks {
    // TODO better lifetime management for turbo tasks
    // consider using unsafe for the task_local turbo tasks
    // that should be safe as long tasks can't outlife turbo task
    // so we probably want to make sure that all tasks are joined
    // when trying to drop turbo tasks
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            resolve_task_cache: CHashMap::new(),
            native_task_cache: CHashMap::new(),
            trait_task_cache: CHashMap::new(),
            currently_scheduled_tasks: AtomicUsize::new(0),
            scheduled_tasks: AtomicUsize::new(0),
            start: Default::default(),
            last_update: Default::default(),
            event: Event::new(),
        })
    }

    pub fn spawn_root_task(
        self: &Arc<Self>,
        functor: impl Fn() -> NativeTaskFuture + Sync + Send + 'static,
    ) -> Arc<Task> {
        let task = Arc::new(Task::new_root(functor));
        self.clone().schedule(task.clone());
        task
    }

    pub fn spawn_once_task(
        self: &Arc<Self>,
        future: impl Future<Output = Result<SlotRef>> + Send + 'static,
    ) -> Arc<Task> {
        let task = Arc::new(Task::new_once(future));
        self.clone().schedule(task.clone());
        task
    }

    fn cached_call<K: PartialEq + Hash>(
        self: &Arc<Self>,
        map: &CHashMap<K, Arc<Task>>,
        key: K,
        create_new: impl FnOnce() -> Task,
    ) -> SlotRef {
        if let Some(cached) = map.get(&key) {
            // fast pass without key lock (only read lock on table)
            let task = cached.clone();
            drop(cached);
            Task::with_current(|parent| task.connect_parent(parent));
            // TODO maybe force (background) scheduling to avoid inactive tasks hanging in "in progress" until they become active
            SlotRef::TaskOutput(task)
        } else {
            // slow pass with key lock
            let new_task = Arc::new(create_new());
            let mut result_task = new_task.clone();
            map.alter(key, |old| match old {
                Some(t) => {
                    result_task = t.clone();
                    Some(t)
                }
                None => {
                    // This is the most likely case
                    // so we want this to be as fast as possible
                    // avoiding locking the map too long
                    Some(new_task)
                }
            });
            let task = result_task;
            Task::with_current(|parent| task.connect_parent(parent));
            SlotRef::TaskOutput(task)
        }
    }

    pub(crate) fn native_call(
        self: &Arc<Self>,
        func: &'static NativeFunction,
        inputs: Vec<TaskInput>,
    ) -> SlotRef {
        debug_assert!(inputs.iter().all(|i| i.is_resolved() && !i.is_nothing()));
        self.cached_call(&self.native_task_cache, (func, inputs.clone()), || {
            Task::new_native(inputs, func)
        })
    }

    pub fn dynamic_call(
        self: &Arc<Self>,
        func: &'static NativeFunction,
        inputs: Vec<TaskInput>,
    ) -> SlotRef {
        if inputs.iter().all(|i| i.is_resolved() && !i.is_nothing()) {
            self.native_call(func, inputs)
        } else {
            self.cached_call(&self.resolve_task_cache, (func, inputs.clone()), || {
                Task::new_resolve_native(inputs, func)
            })
        }
    }

    pub fn trait_call(
        self: &Arc<Self>,
        trait_type: &'static TraitType,
        trait_fn_name: String,
        inputs: Vec<TaskInput>,
    ) -> SlotRef {
        self.cached_call(
            &self.trait_task_cache,
            (trait_type, trait_fn_name.clone(), inputs.clone()),
            || Task::new_resolve_trait(trait_type, trait_fn_name, inputs),
        )
    }

    pub(crate) fn schedule(self: Arc<Self>, task: Arc<Task>) -> JoinHandle<()> {
        if self
            .currently_scheduled_tasks
            .fetch_add(1, Ordering::AcqRel)
            == 0
        {
            *self.start.lock().unwrap() = Some(Instant::now());
        }
        self.scheduled_tasks.fetch_add(1, Ordering::AcqRel);
        Builder::new()
            // that's expensive
            // .name(format!("{:?} {:?}", &*task, &*task as *const Task))
            .spawn(async move {
                if task.execution_started(&self) {
                    Task::set_current(task.clone());
                    let tt = self.clone();
                    TURBO_TASKS.with(|c| (*c.borrow_mut()) = Some(tt));
                    let result = task.execute(self.clone()).await;
                    if let Err(err) = &result {
                        println!("Task {} errored  {}", task, err);
                    }
                    task.execution_result(result);
                    TASKS_TO_NOTIFY.with(|tasks| {
                        for task in tasks.take().iter() {
                            task.dependent_slot_updated(self.clone());
                        }
                    });
                    task.execution_completed(self.clone());
                }
                if self
                    .currently_scheduled_tasks
                    .fetch_sub(1, Ordering::AcqRel)
                    == 1
                {
                    // That's not super race-condition-safe, but it's only for statistical reasons
                    let total = self.scheduled_tasks.load(Ordering::Acquire);
                    self.scheduled_tasks.store(0, Ordering::Release);
                    if let Some(start) = *self.start.lock().unwrap() {
                        *self.last_update.lock().unwrap() = Some((start.elapsed(), total));
                    }
                    self.event.notify(usize::MAX);
                }
            })
            .unwrap()
    }

    pub async fn wait_done(self: &Arc<Self>) -> (Duration, usize) {
        self.event.listen().await;
        self.last_update.lock().unwrap().unwrap()
    }

    pub(crate) fn current() -> Option<Arc<Self>> {
        TURBO_TASKS.with(|c| (*c.borrow()).clone())
    }

    pub(crate) fn schedule_background_job(
        self: Arc<Self>,
        job: impl Future<Output = ()> + Send + 'static,
    ) {
        Builder::new()
            .spawn(async move {
                TURBO_TASKS.with(|c| (*c.borrow_mut()) = Some(self.clone()));
                if self.currently_scheduled_tasks.load(Ordering::Acquire) != 0 {
                    let listener = self.event.listen();
                    if self.currently_scheduled_tasks.load(Ordering::Acquire) != 0 {
                        listener.await;
                    }
                }
                job.await;
            })
            .unwrap();
    }

    pub(crate) fn schedule_notify_tasks(tasks_iter: impl Iterator<Item = Arc<Task>>) {
        TASKS_TO_NOTIFY.with(|tasks| {
            let mut temp = Vec::new();
            tasks.swap(Cell::from_mut(&mut temp));
            for task in tasks_iter {
                temp.push(task);
            }
            tasks.swap(Cell::from_mut(&mut temp));
        });
    }

    pub(crate) fn schedule_deactivate_tasks(self: &Arc<Self>, tasks: Vec<Arc<Task>>) {
        let tt = self.clone();
        self.clone().schedule_background_job(async move {
            Task::deactivate_tasks(tasks, tt);
        });
    }

    pub(crate) fn schedule_remove_tasks(self: &Arc<Self>, tasks: HashSet<Arc<Task>>) {
        let tt = self.clone();
        self.clone().schedule_background_job(async move {
            Task::remove_tasks(tasks, tt);
        });
    }

    pub fn cached_tasks_iter(&self) -> impl Iterator<Item = Arc<Task>> {
        let mut tasks = Vec::new();
        for (_, task) in self.resolve_task_cache.clone().into_iter() {
            tasks.push(task);
        }
        for (_, task) in self.native_task_cache.clone().into_iter() {
            tasks.push(task);
        }
        for (_, task) in self.trait_task_cache.clone().into_iter() {
            tasks.push(task);
        }
        tasks.into_iter()
    }
}

pub fn dynamic_call(func: &'static NativeFunction, inputs: Vec<TaskInput>) -> SlotRef {
    let tt = TurboTasks::current()
        .ok_or_else(|| anyhow!("tried to call dynamic_call outside of turbo tasks"))
        .unwrap();
    tt.dynamic_call(func, inputs)
}

pub fn trait_call(
    trait_type: &'static TraitType,
    trait_fn_name: String,
    inputs: Vec<TaskInput>,
) -> SlotRef {
    let tt = TurboTasks::current()
        .ok_or_else(|| anyhow!("tried to call trait_call outside of turbo tasks"))
        .unwrap();
    tt.trait_call(trait_type, trait_fn_name, inputs)
}