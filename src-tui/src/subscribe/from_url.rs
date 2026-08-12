// Stub for PrfItem::from_url adapter (replaces upstream NetworkManager).
// Full implementation in plan 03-01 task 2.

use clash_verge_core::config::PrfItem;

use super::fetch;

/// Create a PrfItem by fetching and parsing a subscription URL.
///
/// Subscription APIs return a complete Clash config YAML (with `proxies`,
/// `proxy-groups`, `rules`, etc.), not a list of PrfItem structs. We save the
/// raw response body to disk so Mihomo can load it, and record the profile
/// metadata in profiles.yaml.
pub async fn from_url(url: &str, name: &str) -> anyhow::Result<PrfItem> {
    let result = fetch::fetch_subscription(url, None, &[]).await?;
    let body = result.body;

    // Validate that the response looks like a Clash config (not HTML error page
    // or empty body).
    if body.trim().is_empty() {
        anyhow::bail!("subscription returned an empty response");
    }
    if body.trim().starts_with('<') {
        anyhow::bail!(
            "subscription returned HTML (possible 403/block or invalid URL): {}",
            body.trim().chars().take(200).collect::<String>()
        );
    }

    // Try parsing as a Clash config mapping to validate it's legitimate.
    // Most subscription APIs return a full config YAML, not a PrfItem list.
    let (is_valid, decoded_body) = {
        if serde_yaml_ng::from_str::<serde_yaml_ng::Mapping>(&body).is_ok() {
            (true, None)
        } else {
            // Legacy subscription formats return base64-encoded YAML.
            use base64::Engine as _;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(body.trim())
                .unwrap_or_default();
            match String::from_utf8(decoded) {
                Ok(s) if serde_yaml_ng::from_str::<serde_yaml_ng::Mapping>(&s).is_ok() => (true, Some(s)),
                _ => (false, None),
            }
        }
    };

    Ok(RemoteProfileBundle { item, fragments })
}

/// Merge a caller's interval/auto-update overrides into a fallback attempt,
/// keeping the attempt's proxy strategy.
fn merge_import_option(base: &PrfOption, user: Option<&PrfOption>) -> PrfOption {
    PrfOption::merge(Some(base), user).unwrap_or_else(|| base.clone())
}

pub async fn import_with_fallback(
    url: &str,
    name: Option<&str>,
    user: Option<&PrfOption>,
) -> anyhow::Result<RemoteProfileBundle> {
    let attempts = [
        PrfOption {
            with_proxy: Some(true),
            self_proxy: Some(false),
            ..Default::default()
        },
        PrfOption {
            with_proxy: Some(false),
            self_proxy: Some(true),
            ..Default::default()
        },
        PrfOption::default(),
    ];

    let mut last_err = None;
    for base in &attempts {
        let merged = merge_import_option(base, user);
        match from_url(url, name, None, Some(&merged)).await {
            Ok(bundle) => return Ok(bundle),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("import failed")))
}

/// GUI-style update retries: current options → self_proxy → with_proxy.
pub async fn update_with_fallback(url: &str, option: Option<&PrfOption>) -> anyhow::Result<RemoteProfileBundle> {
    let mut merged = PrfOption::merge(option, None).unwrap_or_default();

    if let Ok(bundle) = from_url(url, None, None, Some(&merged)).await {
        return Ok(bundle);
    }

    let body = decoded_body.unwrap_or(body);

    // Build the profile item.
    let uid = format!("R{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    let file = format!("{uid}.yaml");

    let item = PrfItem {
        uid: Some(uid.into()),
        name: Some(name.into()),
        url: Some(url.into()),
        itype: Some("remote".into()),
        file: Some(file.into()),
        file_data: Some(body.into()),
        updated: Some(chrono::Utc::now().timestamp() as usize),
        ..Default::default()
    };

    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_requires_proxies_or_providers() {
        assert!(validate_clash_yaml("proxies: []\n").is_ok());
        assert!(validate_clash_yaml("proxy-providers: {}\n").is_ok());
        assert!(validate_clash_yaml("port: 7890\n").is_err());
        assert!(validate_clash_yaml("not: [yaml").is_err());
    }

    #[test]
    fn fix_dirty_url_moves_ampersand_query() {
        let fixed = match fix_dirty_url("https://example.com/path&token=abc&flag=1") {
            Ok(url) => url,
            Err(error) => panic!("dirty url with ampersand query is fixable: {error}"),
        };
        assert_eq!(fixed.path(), "/path");
        assert!(fixed.query().unwrap().contains("token=abc"));
    }

    #[test]
    fn userinfo_header_variants_parse() {
        let mut headers = HashMap::new();
        headers.insert(
            "subscription-userinfo".into(),
            "upload=1; download=2; total=3; expire=4".into(),
        );
        let extra = match parse_subscription_userinfo(&headers) {
            Some(extra) => extra,
            None => panic!("subscription-userinfo header present"),
        };
        assert_eq!(extra.upload, 1);
        assert_eq!(extra.download, 2);
        assert_eq!(extra.total, 3);
        assert_eq!(extra.expire, 4);

        headers.clear();
        headers.insert(
            "x-amz-meta-subscription-userinfo".into(),
            "upload=10; download=20; total=30; expire=40".into(),
        );
        let extra = match parse_subscription_userinfo(&headers) {
            Some(extra) => extra,
            None => panic!("x-amz-meta-subscription-userinfo header present"),
        };
        assert_eq!(extra.upload, 10);
    }

    #[test]
    fn auto_update_defaults_to_enabled() {
        assert!(allow_auto_update_enabled(None));
        let disabled = PrfOption {
            allow_auto_update: Some(false),
            ..Default::default()
        };
        assert!(!allow_auto_update_enabled(Some(&disabled)));
    }

    #[test]
    fn import_option_merge_keeps_strategy_and_takes_user_flags() {
        let base = PrfOption {
            with_proxy: Some(true),
            self_proxy: Some(false),
            ..Default::default()
        };
        // CLI `--update-interval 15 --no-auto-update`.
        let user = PrfOption {
            update_interval: Some(15),
            allow_auto_update: Some(false),
            ..Default::default()
        };

        let merged = merge_import_option(&base, Some(&user));
        // The attempt's proxy strategy survives...
        assert_eq!(merged.with_proxy, Some(true));
        assert_eq!(merged.self_proxy, Some(false));
        // ...while the caller's flags win.
        assert_eq!(merged.update_interval, Some(15));
        assert_eq!(merged.allow_auto_update, Some(false));

        // Without user flags the attempt stands alone.
        let bare = merge_import_option(&base, None);
        assert_eq!(bare.update_interval, None);
        assert_eq!(bare.allow_auto_update, None);
    }
}
