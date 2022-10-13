use anyhow::{anyhow, Result};
use lazy_static::lazy_static;
use swc_core::{
    common::DUMMY_SP,
    ecma::ast::{Expr, ExprStmt, Ident, Lit, Module, ModuleItem, Program, Script, Stmt},
    quote,
};
use turbo_tasks::{primitives::StringVc, Value, ValueToString, ValueToStringVc};
use turbopack_core::{
    asset::Asset,
    chunk::{
        ChunkableAssetReference, ChunkableAssetReferenceVc, ChunkingContextVc, ChunkingType,
        ChunkingTypeOptionVc, ModuleId,
    },
    reference::{AssetReference, AssetReferenceVc},
    resolve::{origin::ResolveOriginVc, parse::RequestVc, ResolveResultVc},
};

use crate::{
    analyzer::imports::ImportAnnotations,
    chunk::EcmascriptChunkPlaceableVc,
    code_gen::{CodeGenerateable, CodeGenerateableVc, CodeGeneration, CodeGenerationVc},
    create_visitor, magic_identifier,
    resolve::esm_resolve,
};

#[turbo_tasks::value]
pub enum ReferencedAsset {
    Some(EcmascriptChunkPlaceableVc),
    None,
}

pub(super) async fn get_ident(asset: EcmascriptChunkPlaceableVc) -> Result<String> {
    let path = asset.path().to_string().await?;
    Ok(magic_identifier::encode(&format!(
        "imported module {}",
        path
    )))
}

#[turbo_tasks::value]
#[derive(Hash, Debug)]
pub struct EsmAssetReference {
    pub origin: ResolveOriginVc,
    pub request: RequestVc,
    pub annotations: ImportAnnotations,
}

impl EsmAssetReference {
    fn get_origin(&self) -> ResolveOriginVc {
        let mut origin = self.origin;
        if let Some(transition) = self.annotations.transition() {
            origin = origin.with_transition(transition);
        }
        origin
    }
}

#[turbo_tasks::value_impl]
impl EsmAssetReferenceVc {
    #[turbo_tasks::function]
    pub(super) async fn get_referenced_asset(self) -> Result<ReferencedAssetVc> {
        let this = self.await?;
        let assets = esm_resolve(this.get_origin(), this.request).primary_assets();
        for asset in assets.await?.iter() {
            if let Some(placeable) = EcmascriptChunkPlaceableVc::resolve_from(asset).await? {
                return Ok(ReferencedAssetVc::cell(ReferencedAsset::Some(placeable)));
            }
        }
        Ok(ReferencedAssetVc::cell(ReferencedAsset::None))
    }

    #[turbo_tasks::function]
    pub fn new(
        origin: ResolveOriginVc,
        request: RequestVc,
        annotations: Value<ImportAnnotations>,
    ) -> Self {
        Self::cell(EsmAssetReference {
            origin,
            request,
            annotations: annotations.into_value(),
        })
    }
}

#[turbo_tasks::value_impl]
impl AssetReference for EsmAssetReference {
    #[turbo_tasks::function]
    fn resolve_reference(&self) -> ResolveResultVc {
        esm_resolve(self.get_origin(), self.request)
    }
}

#[turbo_tasks::value_impl]
impl ValueToString for EsmAssetReference {
    #[turbo_tasks::function]
    async fn to_string(&self) -> Result<StringVc> {
        Ok(StringVc::cell(format!(
            "import {} {}",
            self.request.to_string().await?,
            self.annotations
        )))
    }
}

#[turbo_tasks::value_impl]
impl ChunkableAssetReference for EsmAssetReference {
    #[turbo_tasks::function]
    fn chunking_type(&self, _context: ChunkingContextVc) -> Result<ChunkingTypeOptionVc> {
        Ok(
            if let Some(chunking_type) = self.annotations.chunking_type() {
                match chunking_type {
                    "separate" => ChunkingTypeOptionVc::cell(Some(ChunkingType::Separate)),
                    "parallel" => ChunkingTypeOptionVc::cell(Some(ChunkingType::Parallel)),
                    _ => return Err(anyhow!("unknown chunking_type: {}", chunking_type)),
                }
            } else {
                ChunkingTypeOptionVc::cell(Some(ChunkingType::default()))
            },
        )
    }
}

#[turbo_tasks::value_impl]
impl CodeGenerateable for EsmAssetReference {
    #[turbo_tasks::function]
    async fn code_generation(
        self_vc: EsmAssetReferenceVc,
        context: ChunkingContextVc,
    ) -> Result<CodeGenerationVc> {
        let mut visitors = Vec::new();

        let chunking_type = self_vc.chunking_type(context).await?;

        // separate chunks can't be imported as the modules are not available
        if !matches!(*chunking_type, None | Some(ChunkingType::Separate)) {
            if let ReferencedAsset::Some(asset) = &*self_vc.get_referenced_asset().await? {
                let ident = get_ident(*asset).await?;
                let id = asset.as_chunk_item(context).id().await?;
                visitors.push(create_visitor!(visit_mut_program(program: &mut Program) {
                    let stmt = quote!(
                        "var $name = __turbopack_import__($id);" as Stmt,
                        name = Ident::new(ident.clone().into(), DUMMY_SP),
                        id: Expr = Expr::Lit(match &*id {
                            ModuleId::String(s) => s.clone().into(),
                            ModuleId::Number(n) => (*n as f64).into(),
                        })
                    );
                    insert_hoisted_stmt(program, stmt);
                }));
            }
        }

        Ok(CodeGeneration { visitors }.into())
    }
}

lazy_static! {
    static ref ESM_HOISTING_LOCATION: &'static str = Box::leak(Box::new(magic_identifier::encode(
        "ecmascript hoisting location"
    )));
}

pub(crate) fn insert_hoisted_stmt(program: &mut Program, stmt: Stmt) {
    match program {
        Program::Module(Module { body, .. }) => {
            let pos = body.iter().position(|item| {
                if let ModuleItem::Stmt(Stmt::Expr(ExprStmt {
                    expr: box Expr::Lit(Lit::Str(s)),
                    ..
                })) = item
                {
                    &*s.value == *ESM_HOISTING_LOCATION
                } else {
                    false
                }
            });
            if let Some(pos) = pos {
                body.insert(pos, ModuleItem::Stmt(stmt));
            } else {
                body.insert(
                    0,
                    ModuleItem::Stmt(Stmt::Expr(ExprStmt {
                        expr: box Expr::Lit(Lit::Str((*ESM_HOISTING_LOCATION).into())),
                        span: DUMMY_SP,
                    })),
                );
                body.insert(0, ModuleItem::Stmt(stmt));
            }
        }
        Program::Script(Script { body, .. }) => {
            let pos = body.iter().position(|item| {
                if let Stmt::Expr(ExprStmt {
                    expr: box Expr::Lit(Lit::Str(s)),
                    ..
                }) = item
                {
                    &*s.value == *ESM_HOISTING_LOCATION
                } else {
                    false
                }
            });
            if let Some(pos) = pos {
                body.insert(pos, stmt);
            } else {
                body.insert(
                    0,
                    Stmt::Expr(ExprStmt {
                        expr: box Expr::Lit(Lit::Str((*ESM_HOISTING_LOCATION).into())),
                        span: DUMMY_SP,
                    }),
                );
                body.insert(0, stmt);
            }
        }
    }
}