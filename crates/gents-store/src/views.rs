
use gents_protocol::document_query::{QuerySet, Rendered};
use gents_protocol::views::View;
use gents_schemas::Collection;
use serde_json::Value;

use crate::config_client::{ConfigAccess, ConfigApplyTxn, ConfigClientError};

#[derive(Debug, thiserror::Error)]
pub enum ViewError {
    #[error("reading a view requires an explicit actor")]
    ActorRequired,
    #[error(transparent)]
    Execute(#[from] ConfigClientError),
    #[error("view response has no data object")]
    InvalidResponse,
    #[error("decoding {collection} view rows")]
    Decode {
        collection: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

impl crate::config_client::TransactionFailure for ViewError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Execute(error) => Some(error),
            Self::ActorRequired | Self::InvalidResponse | Self::Decode { .. } => None,
        }
    }
}

/// DefraDB may omit unreadable roots after ACP revocation without returning
/// an error. Missing and null roots therefore represent empty reader-visible
/// results; malformed present rows remain decoding failures.
fn decode<V>(view: &V, response: &Value, root: &str) -> Result<Vec<V::Row>, ViewError>
where
    V: View,
{
    let collection = V::Collection::NAME;
    let data = response.get("data").unwrap_or(response);
    if !data.is_object() {
        return Err(ViewError::InvalidResponse);
    }
    let value = match data.get(root) {
        None | Some(Value::Null) => return Ok(view.finish(Vec::new())),
        Some(value @ Value::Number(_)) => Value::Array(vec![value.clone()]),
        Some(value) => value.clone(),
    };
    let rows =
        serde_json::from_value(value).map_err(|source| ViewError::Decode { collection, source })?;
    Ok(view.finish(rows))
}

pub trait ViewSet {
    type Rows;

    fn render(&self, reader_did: &str) -> Rendered;
    fn decode(&self, response: &Value) -> Result<Self::Rows, ViewError>;
}

macro_rules! view_sets {
    (@impl $(($view:ident, $index:tt, $root:literal)),+) => {
        impl<$($view),+> ViewSet for ($($view,)+)
        where
            $($view: View,)+
        {
            type Rows = ($(Vec<$view::Row>,)+);

            fn render(&self, reader_did: &str) -> Rendered {
                QuerySet::new()
                    $(.root($root, &self.$index.query(reader_did)))+
                    .render()
            }

            fn decode(&self, response: &Value) -> Result<Self::Rows, ViewError> {
                Ok(($(decode(&self.$index, response, $root)?,)+))
            }
        }
    };
    (@expand [$($prefix:tt)*]) => {};
    (@expand [$($prefix:tt)*] $next:tt $(, $rest:tt)*) => {
        view_sets!(@impl $($prefix)* $next);
        view_sets!(@expand [$($prefix)* $next,] $($rest),*);
    };
    ($($entry:tt),+) => {
        view_sets!(@expand [] $($entry),+);
    };
}

view_sets!(
    (V0, 0, "view_0"),
    (V1, 1, "view_1"),
    (V2, 2, "view_2"),
    (V3, 3, "view_3"),
    (V4, 4, "view_4"),
    (V5, 5, "view_5"),
    (V6, 6, "view_6"),
    (V7, 7, "view_7"),
    (V8, 8, "view_8"),
    (V9, 9, "view_9"),
    (V10, 10, "view_10"),
    (V11, 11, "view_11"),
    (V12, 12, "view_12"),
    (V13, 13, "view_13"),
    (V14, 14, "view_14"),
    (V15, 15, "view_15"),
    (V16, 16, "view_16"),
    (V17, 17, "view_17"),
    (V18, 18, "view_18"),
    (V19, 19, "view_19"),
    (V20, 20, "view_20"),
    (V21, 21, "view_21"),
    (V22, 22, "view_22"),
    (V23, 23, "view_23"),
    (V24, 24, "view_24"),
    (V25, 25, "view_25"),
    (V26, 26, "view_26"),
    (V27, 27, "view_27"),
    (V28, 28, "view_28"),
    (V29, 29, "view_29"),
    (V30, 30, "view_30"),
    (V31, 31, "view_31")
);

impl ConfigAccess {
    pub async fn read_view<V>(&self, view: &V) -> Result<Vec<V::Row>, ViewError>
    where
        V: View,
    {
        self.read_views(&(view,)).await.map(|(rows,)| rows)
    }

    pub async fn read_views<V>(&self, views: &V) -> Result<V::Rows, ViewError>
    where
        V: ViewSet,
    {
        let actor = self.actor_did().ok_or(ViewError::ActorRequired)?;
        let query = views.render(&actor);
        let response = self
            .execute_with_variables(&query.document, &query.variables)
            .await?;
        views.decode(&response)
    }
}

impl ConfigApplyTxn<'_> {
    pub async fn read_view<V>(&self, view: &V) -> Result<Vec<V::Row>, ViewError>
    where
        V: View,
    {
        self.read_views(&(view,)).await.map(|(rows,)| rows)
    }

    pub async fn read_views<V>(&self, views: &V) -> Result<V::Rows, ViewError>
    where
        V: ViewSet,
    {
        let actor = self.actor_did().ok_or(ViewError::ActorRequired)?;
        let query = views.render(actor);
        let response = self
            .execute_with_variables(&query.document, &query.variables)
            .await?;
        views.decode(&response)
    }
}
