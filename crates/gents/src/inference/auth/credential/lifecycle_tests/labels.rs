use super::*;

#[tokio::test]
async fn a_new_account_gets_the_next_free_name() {
    let access = access().await;
    for (product, name) in [
        (Product::ChatGpt, "ChatGPT"),
        (Product::Claude, "Claude"),
        (Product::Grok, "Grok"),
    ] {
        let did = did("NextFree", product);
        let a = store(&access, product.sign_in(&did, "a", "refresh-a")).await;
        assert_eq!(
            a.credential.label, None,
            "{product:?}: the original shows {name}"
        );
        let b = store(&access, product.sign_in(&did, "b", "refresh-b")).await;
        assert_eq!(
            b.credential.label.as_deref(),
            Some(format!("{name} 2").as_str())
        );
        let c = store(&access, product.sign_in(&did, "c", "refresh-c")).await;
        assert_eq!(
            c.credential.label.as_deref(),
            Some(format!("{name} 3").as_str())
        );
        labeled(&access, product.sign_in(&did, "b", "refresh-b2"), "Work").await;
        let d = store(&access, product.sign_in(&did, "d", "refresh-d")).await;
        assert_eq!(
            d.credential.label.as_deref(),
            Some(format!("{name} 2").as_str())
        );
        let stored = rows(&access, &did, product.provider()).await;
        let mut labels: Vec<_> = stored.iter().map(|row| row.label.clone()).collect();
        labels.sort();
        let expected = |n: &str| Some(format!("{name}{n}"));
        assert_eq!(
            labels,
            {
                let mut expected = vec![
                    None,
                    expected(" 2"),
                    expected(" 3"),
                    Some("Work".to_string()),
                ];
                expected.sort();
                expected
            },
            "{product:?}"
        );
    }
}

#[tokio::test]
async fn a_label_names_and_a_refresh_renames() {
    let access = access().await;
    for product in PRODUCTS {
        let did = did("Rename", product);
        let signed = labeled(&access, product.sign_in(&did, "a", "refresh-1"), "Work").await;
        assert_eq!(signed.credential.label.as_deref(), Some("Work"));
        let signed = store(&access, product.sign_in(&did, "a", "refresh-2")).await;
        assert_eq!(signed.credential.label.as_deref(), Some("Work"));
        assert_eq!(
            rows(&access, &did, product.provider()).await[0]
                .label
                .as_deref(),
            Some("Work")
        );
        let signed = labeled(&access, product.sign_in(&did, "a", "refresh-3"), "Personal").await;
        assert_eq!(signed.result, SignInResult::Refreshed);
        let stored = rows(&access, &did, product.provider()).await;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].label.as_deref(), Some("Personal"), "{product:?}");
    }
}

#[tokio::test]
async fn labels_are_unique_per_provider_and_clean() {
    let access = access().await;
    let did = "did:key:z6MkTestLabels";
    labeled(
        &access,
        Product::Claude.sign_in(did, "a", "refresh-1"),
        "Work",
    )
    .await;
    for taken in ["Work", " Work "] {
        let error = store_sign_in(
            &access,
            Product::Claude.sign_in(did, "b", "refresh-2"),
            Some(taken),
        )
        .await
        .expect_err("a duplicate label");
        assert!(error.to_string().contains("Work"), "{error}");
    }
    assert_eq!(rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.len(), 1);
    labeled(
        &access,
        Product::Grok.sign_in(did, "a", "refresh-1"),
        "Work",
    )
    .await;

    let long = "x".repeat(65);
    for bad in ["", "   ", long.as_str(), "Work\nmore", "tab\there"] {
        assert!(
            store_sign_in(
                &access,
                Product::ChatGpt.sign_in(did, "a", "refresh-1"),
                Some(bad)
            )
            .await
            .is_err(),
            "{bad:?}"
        );
    }
    assert!(rows(&access, did, CHATGPT_CODEX_PROVIDER).await.is_empty());
    let max = "x".repeat(64);
    labeled(
        &access,
        Product::ChatGpt.sign_in(did, "a", "refresh-1"),
        &max,
    )
    .await;
}
