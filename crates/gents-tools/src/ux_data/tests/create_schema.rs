use super::*;
use crate::ux_data::create_schema::{UxCreateSchemaError, UxCreateSchemaRefusal};

#[tokio::test]
async fn packaged_page_schema_refusals_name_the_page_collection_and_repair() {
    let cases: &[&[(&str, &str)]] = &[
        &[],
        &[("schemas/other.graphql", "type Other { value: String }")],
        &[(
            "schemas/board.graphql",
            "type BoardTask { title: String lane: String } type Other { value: String }",
        )],
        &[
            (
                "schemas/board.graphql",
                "type BoardTask { title: String lane: String }",
            ),
            (
                "schemas/copy.graphql",
                "type BoardTask { title: String lane: String }",
            ),
        ],
    ];
    for (index, schemas) in cases.iter().enumerate() {
        let fixture = Fixture::new().await;
        install_verified_board_schemas(fixture.home.path(), schemas).await;
        let before = fixture
            .access
            .execute("{ BoardTask { _docID title lane } }")
            .await
            .unwrap();
        let error = call(
            fixture.home.path(),
            &fixture.client_access,
            UxDataCall {
                ux: BOARD.into(),
                node_did: fixture.node_did.clone(),
                surface: "board".into(),
                tool: "add_task".into(),
                args: json!({"title":"must-not-sign", "lane":"doing"}),
            },
        )
        .await
        .unwrap_err();
        let typed = error
            .downcast_ref::<UxCreateSchemaError>()
            .expect("a typed schema refusal, not a transaction-owner error");
        let UxCreateSchemaError::Refused {
            page,
            collection,
            reason,
        } = typed
        else {
            panic!("{typed}")
        };
        assert_eq!(page, BOARD);
        assert_eq!(collection, "BoardTask");
        match (index, reason) {
            (0 | 1, UxCreateSchemaRefusal::CollectionNotDeclared { pack }) => {
                assert_eq!(pack, "acme/board")
            }
            (2, UxCreateSchemaRefusal::MultipleCollections { pack, path, count }) => {
                assert_eq!(pack, "acme/board");
                assert_eq!(path, "schemas/board.graphql");
                assert_eq!(*count, 2);
            }
            (
                3,
                UxCreateSchemaRefusal::DuplicateCollection {
                    pack,
                    first,
                    second,
                },
            ) => {
                assert_eq!(pack, "acme/board");
                assert_eq!(first, "schemas/board.graphql");
                assert_eq!(second, "schemas/copy.graphql");
            }
            _ => panic!("unexpected refusal: {typed}"),
        }
        let message = typed.to_string();
        for expected in [BOARD, "BoardTask", "acme/board", "rebuild and reinstall"] {
            assert!(message.contains(expected), "{message}");
        }
        if index == 2 {
            assert!(
                message.contains("one collection per schema file"),
                "{message}"
            );
        }
        assert_eq!(
            fixture
                .access
                .execute("{ BoardTask { _docID title lane } }")
                .await
                .unwrap(),
            before
        );
        fixture.node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn missing_pack_artifacts_have_schema_specific_refusals() {
    let home = tempfile::tempdir().unwrap();
    install_board(home.path(), vec![declared("board", &["add_task"])]);
    let listing = gents_pack::plugin::ux::find(home.path(), BOARD).unwrap();
    let error = trusted_ux_create_schema(home.path(), &listing, "BoardTask")
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        UxCreateSchemaError::Refused {
            reason: UxCreateSchemaRefusal::ArchiveMissing { .. },
            ..
        }
    ));
    let mut orphan = listing.clone();
    orphan.pack = None;
    let error = trusted_ux_create_schema(home.path(), &orphan, "BoardTask")
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        UxCreateSchemaError::Refused {
            reason: UxCreateSchemaRefusal::NoPack,
            ..
        }
    ));
    let empty = tempfile::tempdir().unwrap();
    let error = trusted_ux_create_schema(empty.path(), &listing, "BoardTask")
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        UxCreateSchemaError::Refused {
            reason: UxCreateSchemaRefusal::PackNotInstalled { .. },
            ..
        }
    ));
}
