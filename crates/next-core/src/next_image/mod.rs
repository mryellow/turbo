use std::collections::HashSet;

use anyhow::Result;
use turbo_tasks::{primitives::StringVc, Value};
use turbopack_core::introspect::{Introspectable, IntrospectableChildrenVc, IntrospectableVc};
use turbopack_dev_server::source::{
    query::QueryValue, ContentSource, ContentSourceData, ContentSourceDataFilter,
    ContentSourceDataVary, ContentSourceResult, ContentSourceResultVc, ContentSourceVc,
    ProxyResult,
};

/// Serves, resizes, optimizes, and re-encodes images to be used with
/// next/image.
#[turbo_tasks::value(shared)]
pub struct NextImageContentSource {
    asset_source: ContentSourceVc,
}

#[turbo_tasks::value_impl]
impl NextImageContentSourceVc {
    #[turbo_tasks::function]
    pub fn new(asset_source: ContentSourceVc) -> NextImageContentSourceVc {
        NextImageContentSource { asset_source }.cell()
    }
}

#[turbo_tasks::value_impl]
impl ContentSource for NextImageContentSource {
    #[turbo_tasks::function]
    async fn get(
        self_vc: NextImageContentSourceVc,
        path: &str,
        data: Value<ContentSourceData>,
    ) -> Result<ContentSourceResultVc> {
        let this = self_vc.await?;

        let query = match &data.query {
            None => {
                let queries = [
                    "url".to_string(),
                    // TODO: support q and w queries.
                ]
                .iter()
                .cloned()
                .collect::<HashSet<_>>();

                return Ok(ContentSourceResult::NeedData {
                    source: self_vc.into(),
                    path: path.to_string(),
                    vary: ContentSourceDataVary {
                        url: true,
                        query: Some(ContentSourceDataFilter::Subset(queries)),
                        ..Default::default()
                    },
                }
                .cell());
            }
            Some(query) => query,
        };

        let url = match query.get("url") {
            Some(QueryValue::String(s)) => s,
            _ => return Ok(ContentSourceResult::NotFound.cell()),
        };

        // TODO: consume the assets, resize and reduce quality, re-encode into next-gen
        // formats.
        if let Some(path) = url.strip_prefix('/') {
            let asset = this.asset_source.get(path, Value::new(Default::default()));
            if matches!(&*asset.await?, ContentSourceResult::Static(..)) {
                return Ok(asset);
            }
        }

        // TODO: This should be downloaded by the server, and resized, etc.
        Ok(ContentSourceResult::HttpProxy(
            ProxyResult {
                status: 302,
                headers: vec!["Location".to_string(), url.clone()],
                body: vec![],
            }
            .cell(),
        )
        .cell())
    }
}

#[turbo_tasks::value_impl]
impl Introspectable for NextImageContentSource {
    #[turbo_tasks::function]
    fn ty(&self) -> StringVc {
        StringVc::cell("next image content source".to_string())
    }

    #[turbo_tasks::function]
    fn details(&self) -> StringVc {
        StringVc::cell("suports dynamic serving of any statically imported image".to_string())
    }

    #[turbo_tasks::function]
    async fn children(&self) -> Result<IntrospectableChildrenVc> {
        Ok(IntrospectableChildrenVc::cell(HashSet::new()))
    }
}
