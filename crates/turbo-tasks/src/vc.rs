use std::{any::Any, future::IntoFuture, marker::PhantomData, pin::Pin};

use anyhow::Result;
use lazy_static::lazy_static;

use crate::{
    task::{match_previous_node_by_key, match_previous_node_by_type},
    trace::{TraceSlotVcs, TraceSlotVcsContext},
    SlotValueType, SlotVc, SlotVcReadResult, TaskInput,
};

#[derive(PartialEq, Eq, Clone)]
pub struct Vc<T: Any + TraceSlotVcs + Send + Sync> {
    node: SlotVc,
    phantom_data: PhantomData<T>,
}

impl<T: Any + TraceSlotVcs + Send + Sync> Vc<T> {
    /// Reads the value of the reference.
    ///
    /// This is async and will rethrow any fatal error that happened during task
    /// execution.
    ///
    /// Reading the value will make the current task depend on the slot and the
    /// task outputs. This will lead to invalidation of the current task
    /// when one of these changes.
    pub async fn get(&self) -> Result<SlotVcReadResult<T>> {
        self.node.clone().into_read::<T>().await
    }

    /// Resolve the reference until it points to a slot directly.
    ///
    /// This is async and will rethrow any fatal error that happened during task
    /// execution.
    pub async fn resolve(self) -> Result<Self> {
        Ok(Self {
            node: self.node.resolve().await?,
            phantom_data: PhantomData,
        })
    }

    fn value_type() -> &'static SlotValueType {
        // TODO create unique value type per T
        lazy_static! {
            static ref VALUE_TYPE: SlotValueType =
                SlotValueType::new("generic promise".to_string());
        }
        &*VALUE_TYPE
    }
}

impl<T: Any + PartialEq + Eq + TraceSlotVcs + Send + Sync> Vc<T> {
    /// Places a value in a slot of the current task.
    /// If there is already a value in the slot it only overrides the value when
    /// it's not equal to the provided value. (Requires `Eq` trait to be
    /// implemented on the type.)
    ///
    /// Slot is selected based on the value type and call order of `slot`.
    pub fn slot(content: T) -> Self {
        Self {
            node: match_previous_node_by_type::<T, _>(|__slot| {
                __slot.compare_and_update_shared(&Self::value_type(), content);
            }),
            phantom_data: PhantomData,
        }
    }
}

impl<
        T: std::hash::Hash + std::cmp::PartialEq + std::cmp::Eq + TraceSlotVcs + Send + Sync + 'static,
    > Vc<T>
{
    /// Places a value in a slot of the current task.
    /// If there is already a value in the slot it only overrides the value when
    /// it's not equal to the provided value. (Requires `Eq` trait to be
    /// implemented on the type.)
    ///
    /// Slot is selected by the provided `key`. `key` must not be used twice
    /// during the current task.
    pub fn keyed_slot(key: T, content: T) -> Self {
        Self {
            node: match_previous_node_by_key::<T, T, _>(key, |__slot| {
                __slot.compare_and_update_shared(&Self::value_type(), content);
            }),
            phantom_data: PhantomData,
        }
    }
}

impl<T: Any + TraceSlotVcs + Send + Sync> From<SlotVc> for Vc<T> {
    fn from(node: SlotVc) -> Self {
        Self {
            node,
            phantom_data: PhantomData,
        }
    }
}

impl<T: Any + TraceSlotVcs + Send + Sync> From<Vc<T>> for SlotVc {
    fn from(node_ref: Vc<T>) -> Self {
        node_ref.node
    }
}

impl<T: Any + TraceSlotVcs + Send + Sync> From<&Vc<T>> for SlotVc {
    fn from(node_ref: &Vc<T>) -> Self {
        node_ref.node.clone()
    }
}

impl<T: Any + TraceSlotVcs + Send + Sync> From<Vc<T>> for TaskInput {
    fn from(node_ref: Vc<T>) -> Self {
        node_ref.node.into()
    }
}

impl<T: Any + TraceSlotVcs + Send + Sync> From<&Vc<T>> for TaskInput {
    fn from(node_ref: &Vc<T>) -> Self {
        node_ref.node.clone().into()
    }
}

impl<T: Any + TraceSlotVcs + Send + Sync> TryFrom<&TaskInput> for Vc<T> {
    type Error = anyhow::Error;

    fn try_from(value: &TaskInput) -> Result<Self, Self::Error> {
        Ok(Self {
            node: value.try_into()?,
            phantom_data: PhantomData,
        })
    }
}

impl<T: Any + TraceSlotVcs + Send + Sync> TraceSlotVcs for Vc<T> {
    fn trace_node_refs(&self, context: &mut TraceSlotVcsContext) {
        TraceSlotVcs::trace_node_refs(&self.node, context);
    }
}

impl<T: Any + TraceSlotVcs + Send + Sync> IntoFuture for Vc<T> {
    type Output = Result<SlotVcReadResult<T>>;

    type IntoFuture = Pin<
        Box<dyn std::future::Future<Output = Result<SlotVcReadResult<T>>> + Send + Sync + 'static>,
    >;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.node.clone().into_read::<T>())
    }
}