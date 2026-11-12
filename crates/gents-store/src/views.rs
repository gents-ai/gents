pub mod journals;
pub mod runtime;

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
fn decode<V>(
    view: &V,
    response: &Value,
    root: &str,
    is_count: bool,
) -> Result<Vec<V::Row>, ViewError>
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
        Some(value @ Value::Number(_)) if is_count => Value::Array(vec![value.clone()]),
        Some(value @ Value::Array(_)) if !is_count => value.clone(),
        Some(_) => {
            return Err(ViewError::Decode {
                collection,
                source: <serde_json::Error as serde::de::Error>::custom(if is_count {
                    "expected a count"
                } else {
                    "expected view rows"
                }),
            });
        }
    };
    let rows =
        serde_json::from_value(value).map_err(|source| ViewError::Decode { collection, source })?;
    Ok(view.finish(rows))
}

pub trait ViewSet {
    type Rows;

    fn render(&self, reader_did: &str) -> Rendered;
    fn decode(&self, response: &Value, reader_did: &str) -> Result<Self::Rows, ViewError>;

    fn is_empty(&self) -> bool {
        false
    }
}

impl<V> ViewSet for [V]
where
    V: View,
{
    type Rows = Vec<Vec<V::Row>>;

    fn render(&self, reader_did: &str) -> Rendered {
        self.iter()
            .enumerate()
            .fold(QuerySet::new(), |queries, (index, view)| {
                queries.indexed(index, &view.query(reader_did))
            })
            .render()
    }

    fn decode(&self, response: &Value, reader_did: &str) -> Result<Self::Rows, ViewError> {
        self.iter()
            .enumerate()
            .map(|(index, view)| {
                decode(
                    view,
                    response,
                    &format!("view_{index}"),
                    view.query(reader_did).is_count(),
                )
            })
            .collect()
    }

    fn is_empty(&self) -> bool {
        self.is_empty()
    }
}

impl<V> ViewSet for Vec<V>
where
    V: View,
{
    type Rows = Vec<Vec<V::Row>>;

    fn render(&self, reader_did: &str) -> Rendered {
        self.as_slice().render(reader_did)
    }

    fn decode(&self, response: &Value, reader_did: &str) -> Result<Self::Rows, ViewError> {
        self.as_slice().decode(response, reader_did)
    }

    fn is_empty(&self) -> bool {
        self.is_empty()
    }
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

            fn decode(&self, response: &Value, reader_did: &str) -> Result<Self::Rows, ViewError> {
                Ok(($(decode(&self.$index, response, $root, self.$index.query(reader_did).is_count())?,)+))
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

pub async fn read_view<R, V>(reader: &R, view: &V) -> Result<Vec<V::Row>, ViewError>
where
    R: crate::config_client::ConfigRead + ?Sized,
    V: View,
{
    read_views(reader, &(view,)).await.map(|(rows,)| rows)
}

pub async fn read_views<R, V>(reader: &R, views: &V) -> Result<V::Rows, ViewError>
where
    R: crate::config_client::ConfigRead + ?Sized,
    V: ViewSet + ?Sized,
{
    let actor = reader.reader_did().ok_or(ViewError::ActorRequired)?;
    if views.is_empty() {
        return views.decode(&serde_json::json!({}), &actor);
    }
    let query = views.render(&actor);
    let response = reader
        .execute_read_with_variables(&query.document, &query.variables)
        .await?;
    views.decode(&response, &actor)
}

impl ConfigAccess {
    pub async fn read_view<V>(&self, view: &V) -> Result<Vec<V::Row>, ViewError>
    where
        V: View,
    {
        read_view(self, view).await
    }

    pub async fn read_views<V>(&self, views: &V) -> Result<V::Rows, ViewError>
    where
        V: ViewSet + ?Sized,
    {
        read_views(self, views).await
    }
}

impl ConfigApplyTxn<'_> {
    pub async fn read_view<V>(&self, view: &V) -> Result<Vec<V::Row>, ViewError>
    where
        V: View,
    {
        read_view(self, view).await
    }

    pub async fn read_views<V>(&self, views: &V) -> Result<V::Rows, ViewError>
    where
        V: ViewSet + ?Sized,
    {
        read_views(self, views).await
    }
}
