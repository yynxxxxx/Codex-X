use super::*;

fn temp_codex_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "codex-x-{name}-{}-{}",
        std::process::id(),
        Local::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    fs::create_dir_all(&dir).expect("create temp codex dir");
    dir
}

#[test]
fn codex_dir_input_expands_home_and_removes_matching_quotes() {
    assert_eq!(
        codex_dir_from_text("~/.codex-custom").expect("expand home"),
        Some(home_dir().expect("home directory").join(".codex-custom"))
    );
    assert_eq!(
        codex_dir_from_text(r#""C:\Users\Test User\.codex""#).expect("remove quotes"),
        Some(PathBuf::from(r"C:\Users\Test User\.codex"))
    );
    assert_eq!(codex_dir_from_text("   ").expect("empty path"), None);
    assert_eq!(
        codex_dir_from_text("\"\"").expect("quoted empty path"),
        None
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_file_link_codex_home_is_followed_again_after_target_switch() {
    use std::os::windows::fs::symlink_file;

    let root = temp_codex_dir("windows-file-link-codex-home");
    let first = root.join("目标一");
    let second = root.join("目标二");
    let link = root.join(".codex");
    fs::create_dir(&first).expect("create first target");
    fs::create_dir(&second).expect("create second target");
    match symlink_file(&first, &link) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(1314) => {
            fs::remove_dir_all(root).expect("remove test directory");
            return;
        }
        Err(error) => panic!("create file link: {error}"),
    }

    enable_prompt_content_inner(
        Some(link.display().to_string()),
        INSTRUCTION_FILENAME,
        "first target prompt",
        "builtin:gpt5.5-unrestricted",
        "managed",
        "test",
        PromptInjectionMode::Replace,
        "test-windows-file-link-first",
    )
    .expect("enable through first file link target");
    assert_eq!(
        fs::read_to_string(first.join(INSTRUCTION_FILENAME)).expect("read first target prompt"),
        "first target prompt"
    );

    fs::remove_file(&link).expect("remove first file link");
    symlink_file(&second, &link).expect("create second file link");
    enable_prompt_content_inner(
        Some(link.display().to_string()),
        INSTRUCTION_FILENAME,
        "second target prompt",
        "builtin:gpt5.5-unrestricted",
        "managed",
        "test",
        PromptInjectionMode::Replace,
        "test-windows-file-link-second",
    )
    .expect("enable through second file link target");
    assert_eq!(
        fs::read_to_string(second.join(INSTRUCTION_FILENAME)).expect("read second target prompt"),
        "second target prompt"
    );

    fs::remove_file(&link).expect("remove second file link");
    symlink_file(PathBuf::from("目标一"), &link).expect("create relative file link");
    assert_eq!(
        resolve_codex_dir(Some(link.display().to_string())).expect("resolve relative target"),
        first
    );
    fs::remove_file(&link).expect("remove relative file link");

    symlink_file(root.join("missing-target"), &link).expect("create broken file link");
    let missing_error = resolve_codex_dir(Some(link.display().to_string()))
        .expect_err("reject missing file-link target");
    assert!(missing_error.to_string().contains("目标不存在"));
    assert!(fs::symlink_metadata(&link).is_ok());
    fs::remove_file(&link).expect("remove broken file link");

    let file_target = root.join("not-a-directory");
    fs::write(&file_target, "keep").expect("create file target");
    symlink_file(&file_target, &link).expect("create file link to file");
    let file_error = resolve_codex_dir(Some(link.display().to_string()))
        .expect_err("reject non-directory file-link target");
    assert!(file_error.to_string().contains("不是文件夹"));
    assert_eq!(
        fs::read_to_string(&file_target).expect("read file target"),
        "keep"
    );
    fs::remove_file(&link).expect("remove non-directory file link");

    let loop_a = root.join("loop-a");
    let loop_b = root.join("loop-b");
    symlink_file(&loop_b, &loop_a).expect("create first loop link");
    symlink_file(&loop_a, &loop_b).expect("create second loop link");
    let loop_error =
        resolve_codex_dir(Some(loop_a.display().to_string())).expect_err("reject file-link loop");
    assert!(loop_error.to_string().contains("形成了循环"));
    fs::remove_file(loop_a).expect("remove first loop link");
    fs::remove_file(loop_b).expect("remove second loop link");

    fs::remove_dir_all(root).expect("remove test directory");
}

fn provider_test_connection() -> Connection {
    let conn = Connection::open_in_memory().expect("open provider test database");
    conn.execute_batch(
        "CREATE TABLE providers (
                id TEXT PRIMARY KEY,
                provider_name TEXT NOT NULL,
                base_url TEXT NOT NULL,
                model TEXT NOT NULL,
                api_key TEXT,
                toml_config TEXT,
                wire_api TEXT NOT NULL DEFAULT 'responses',
                requires_openai_auth INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
    )
    .expect("create providers table");
    conn
}

fn provider_fixture(
    id: &str,
    name: &str,
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
    toml_config: Option<&str>,
) -> SavedProvider {
    SavedProvider {
        id: id.to_string(),
        provider_name: name.to_string(),
        base_url: base_url.to_string(),
        model: model.to_string(),
        api_key: api_key.map(ToString::to_string),
        toml_config: toml_config.map(ToString::to_string),
        wire_api: "responses".to_string(),
        requires_openai_auth: true,
    }
}

fn seed_provider(conn: &Connection, provider: &SavedProvider, created_at: &str, updated_at: &str) {
    conn.execute(
        "INSERT INTO providers
                (id, provider_name, base_url, model, api_key, toml_config, wire_api,
                 requires_openai_auth, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            provider.id,
            provider.provider_name,
            provider.base_url,
            provider.model,
            provider.api_key,
            provider.toml_config,
            provider.wire_api,
            if provider.requires_openai_auth { 1 } else { 0 },
            created_at,
            updated_at,
        ],
    )
    .expect("seed provider");
}

#[test]
fn provider_base_url_canonicalization_preserves_path_case() {
    assert_eq!(
        canonical_provider_base_url("  HTTP://Example.COM:80/V1///  "),
        "http://example.com/V1"
    );
    assert_eq!(
        canonical_provider_base_url("https://EXAMPLE.com:443/v1/#ignored"),
        "https://example.com/v1"
    );
    assert_eq!(
        canonical_provider_base_url("https://example.com:8443/V1/?Region=US#ignored"),
        "https://example.com:8443/V1?Region=US"
    );
}

#[test]
fn provider_identity_uses_url_and_effective_credential_not_model_or_name() {
    let direct = provider_fixture(
        "direct",
        "Magic AI",
        "https://EXAMPLE.com:443/v1/",
        Some("sk-same"),
        "gpt-5.6-sol",
        None,
    );
    let toml = provider_fixture(
        "toml",
        "Renamed Provider",
        "https://example.com/v1",
        None,
        "gpt-5.5",
        Some(
            r#"model_provider = "custom"
[model_providers.custom]
experimental_bearer_token = "sk-same"
"#,
        ),
    );
    let different_key = provider_fixture(
        "different",
        "Magic AI",
        "https://example.com/v1",
        Some("sk-other"),
        "gpt-5.5",
        None,
    );
    assert_eq!(provider_identity(&direct), provider_identity(&toml));
    assert_ne!(
        provider_identity(&direct),
        provider_identity(&different_key)
    );

    let anonymous_a = provider_fixture(
        "anonymous-a",
        "  Acme\u{2003}API  ",
        "https://example.com/v1/",
        None,
        "one",
        None,
    );
    let anonymous_b = provider_fixture(
        "anonymous-b",
        "acme api",
        "https://EXAMPLE.com/v1",
        None,
        "two",
        None,
    );
    assert_eq!(
        provider_identity(&anonymous_a),
        provider_identity(&anonymous_b)
    );
}

#[test]
fn manual_provider_save_upserts_same_url_and_key_but_keeps_different_keys() {
    let conn = provider_test_connection();
    let first = normalize_saved_provider(provider_fixture(
        "first",
        "First Name",
        "https://example.com/v1/",
        Some("sk-same"),
        "model-a",
        None,
    ))
    .expect("normalize first");
    let added =
        upsert_provider_on_connection(&conn, first, ProviderUpsertMode::Manual).expect("add first");
    assert_eq!(added.kind, ProviderUpsertKind::Added);

    let renamed = normalize_saved_provider(provider_fixture(
        "second",
        "Second Name",
        "HTTPS://EXAMPLE.COM:443/v1",
        Some("sk-same"),
        "model-b",
        None,
    ))
    .expect("normalize renamed");
    let merged = upsert_provider_on_connection(&conn, renamed, ProviderUpsertMode::Manual)
        .expect("merge same identity");
    assert_eq!(merged.kind, ProviderUpsertKind::Merged);
    assert_eq!(merged.provider.id, "first");
    assert_eq!(merged.provider.provider_name, "Second Name");
    assert_eq!(merged.provider.model, "model-b");

    let other_key = normalize_saved_provider(provider_fixture(
        "third",
        "Second Name",
        "https://example.com/v1",
        Some("sk-other"),
        "model-b",
        None,
    ))
    .expect("normalize other key");
    let second_add = upsert_provider_on_connection(&conn, other_key, ProviderUpsertMode::Manual)
        .expect("keep different credential");
    assert_eq!(second_add.kind, ProviderUpsertKind::Added);
    assert_eq!(list_saved_providers_on_connection(&conn).unwrap().len(), 2);
}

#[test]
fn imported_provider_merge_preserves_existing_local_profile_and_toml() {
    let conn = provider_test_connection();
    let local_toml = r#"model_provider = "custom"
model = "local-model"
[model_providers.custom]
base_url = "https://example.com/v1"
experimental_bearer_token = "sk-same"
"#;
    let local = normalize_saved_provider(provider_fixture(
        "local",
        "Local Name",
        "https://example.com/v1",
        Some("sk-same"),
        "local-model",
        Some(local_toml),
    ))
    .expect("normalize local");
    upsert_provider_on_connection(&conn, local, ProviderUpsertMode::Manual).expect("save local");

    let imported = normalize_saved_provider(provider_fixture(
        "cc-switch-id",
        "CC Name",
        "https://EXAMPLE.com:443/v1/",
        Some("sk-same"),
        "cc-model",
        None,
    ))
    .expect("normalize import");
    let result =
        upsert_provider_on_connection(&conn, imported.clone(), ProviderUpsertMode::Imported)
            .expect("merge import");
    assert_eq!(result.kind, ProviderUpsertKind::Merged);
    assert_eq!(result.provider.id, "local");
    assert_eq!(result.provider.provider_name, "Local Name");
    assert_eq!(result.provider.model, "local-model");
    assert_eq!(
        result.provider.toml_config.as_deref(),
        Some(local_toml.trim_end())
    );
    assert_eq!(list_saved_providers_on_connection(&conn).unwrap().len(), 1);

    let repeated = upsert_provider_on_connection(&conn, imported, ProviderUpsertMode::Imported)
        .expect("repeat identical import");
    assert_eq!(repeated.provider.id, "local");
    assert_eq!(
        repeated.provider.toml_config.as_deref(),
        Some(local_toml.trim_end())
    );
    assert_eq!(list_saved_providers_on_connection(&conn).unwrap().len(), 1);
}

#[test]
fn provider_migration_merges_only_exact_nonempty_credentials() {
    let mut conn = provider_test_connection();
    let first = provider_fixture(
        "first-id",
        "Local Name",
        "HTTPS://EXAMPLE.com:443/v1/",
        Some("sk-same"),
        "local-model",
        None,
    );
    let duplicate = provider_fixture(
        "later-id",
        "Imported Name",
        "https://example.com/v1",
        Some("sk-same"),
        "imported-model",
        Some("local preserved toml"),
    );
    let different_key = provider_fixture(
        "different-key",
        "Local Name",
        "https://example.com/v1",
        Some("sk-other"),
        "other-model",
        None,
    );
    let anonymous_a = provider_fixture(
        "anonymous-a",
        "No Key",
        "https://example.com/v1",
        None,
        "one",
        None,
    );
    let anonymous_b = provider_fixture(
        "anonymous-b",
        " no   key ",
        "https://example.com/v1/",
        None,
        "two",
        None,
    );
    seed_provider(
        &conn,
        &first,
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:00:00Z",
    );
    seed_provider(
        &conn,
        &duplicate,
        "2026-02-01T00:00:00Z",
        "2026-02-01T00:00:00Z",
    );
    seed_provider(
        &conn,
        &different_key,
        "2026-03-01T00:00:00Z",
        "2026-03-01T00:00:00Z",
    );
    seed_provider(
        &conn,
        &anonymous_a,
        "2026-04-01T00:00:00Z",
        "2026-04-01T00:00:00Z",
    );
    seed_provider(
        &conn,
        &anonymous_b,
        "2026-05-01T00:00:00Z",
        "2026-05-01T00:00:00Z",
    );

    assert_eq!(merge_duplicate_provider_identities(&mut conn).unwrap(), 1);
    let rows = list_saved_providers_on_connection(&conn).unwrap();
    assert_eq!(rows.len(), 4);
    let survivor = rows.iter().find(|row| row.id == "first-id").unwrap();
    assert_eq!(survivor.provider_name, "Local Name");
    assert_eq!(survivor.model, "local-model");
    assert_eq!(
        survivor.toml_config.as_deref(),
        Some("local preserved toml")
    );
    assert!(rows.iter().any(|row| row.id == "different-key"));
    assert!(rows.iter().any(|row| row.id == "anonymous-a"));
    assert!(rows.iter().any(|row| row.id == "anonymous-b"));
    assert!(!rows.iter().any(|row| row.id == "later-id"));
}

#[test]
fn provider_slug_collision_does_not_overwrite_an_unrelated_id() {
    let conn = provider_test_connection();
    let existing = provider_fixture(
        "collision-id",
        "Existing",
        "https://first.example/v1",
        Some("sk-first"),
        "first",
        None,
    );
    seed_provider(
        &conn,
        &existing,
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:00:00Z",
    );
    let collision = provider_fixture(
        "Collision ID",
        "Unrelated",
        "https://second.example/v1",
        Some("sk-second"),
        "second",
        None,
    );
    assert!(save_manual_provider_on_connection(&conn, collision).is_err());
    let stored = provider_by_id_on_connection(&conn, "collision-id")
        .unwrap()
        .unwrap();
    assert_eq!(stored.provider_name, "Existing");
    assert_eq!(stored.base_url, "https://first.example/v1");
    assert_eq!(list_saved_providers_on_connection(&conn).unwrap().len(), 1);
}

#[test]
fn detected_provider_id_collision_gets_a_unique_id() {
    let conn = provider_test_connection();
    let existing = normalize_saved_provider(provider_fixture(
        "custom",
        "Existing",
        "https://first.example/v1",
        Some("sk-first"),
        "first",
        None,
    ))
    .unwrap();
    upsert_provider_on_connection(&conn, existing, ProviderUpsertMode::Manual).unwrap();
    let detected = normalize_saved_provider(provider_fixture(
        "custom",
        "Detected",
        "https://second.example/v1",
        Some("sk-second"),
        "second",
        None,
    ))
    .unwrap();
    let result = upsert_provider_on_connection(&conn, detected, ProviderUpsertMode::Detected)
        .expect("save collision safely");
    assert_eq!(result.kind, ProviderUpsertKind::Added);
    assert_eq!(result.provider.id, "custom-2");
    assert_eq!(list_saved_providers_on_connection(&conn).unwrap().len(), 2);
}

#[test]
fn ccswitch_row_reader_supports_legacy_schema_without_category() {
    let conn = Connection::open_in_memory().expect("open legacy cc-switch database");
    conn.execute_batch(
        "CREATE TABLE providers (
                id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                name TEXT NOT NULL,
                settings_config TEXT NOT NULL,
                sort_index INTEGER,
                created_at INTEGER,
                PRIMARY KEY (id, app_type)
            );
            INSERT INTO providers (id, app_type, name, settings_config, sort_index, created_at)
            VALUES ('legacy', 'codex', 'Legacy', '{}', 0, 1);",
    )
    .expect("seed legacy cc-switch database");
    let rows = read_ccswitch_codex_rows(&conn).expect("read legacy rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "legacy");
    assert_eq!(rows[0].category, None);

    let official = CcSwitchCodexRow {
        id: "codex-official".to_string(),
        name: "OpenAI Official".to_string(),
        settings_config: "{}".to_string(),
        category: None,
    };
    assert!(is_official_ccswitch_row(&official));
}

#[test]
fn test_app_home_is_stable_and_does_not_use_real_codexx_home() {
    let first = app_home().expect("resolve test app home");
    let second = app_home().expect("resolve test app home again");
    let real = home_dir().expect("resolve real home").join(".codexx");

    assert_eq!(first, second);
    assert_ne!(first, real);
    assert!(first.starts_with(std::env::temp_dir()));
}

#[test]
fn skills_and_mcp_order_does_not_depend_on_enabled_state() {
    let skill = |id: &str, name: &str, enabled: bool| ManagedSkill {
        id: id.to_string(),
        name: name.to_string(),
        description: None,
        directory: id.to_string(),
        enabled,
        source: "test".to_string(),
        path: String::new(),
        content_hash: None,
        update_status: String::new(),
    };
    let server = |id: &str, name: &str, enabled: bool| ManagedMcpServer {
        id: id.to_string(),
        name: name.to_string(),
        transport: "stdio".to_string(),
        enabled,
        source: "test".to_string(),
        summary: String::new(),
        command: None,
        url: None,
        config_json: json!({}),
    };
    let mut skills = vec![
        skill("beta", "Beta", true),
        skill("alpha", "alpha", false),
        skill("gamma", "Gamma", true),
    ];
    let mut servers = vec![
        server("beta", "Beta", false),
        server("alpha", "alpha", true),
        server("gamma", "Gamma", false),
    ];

    sort_managed_skills(&mut skills);
    sort_managed_mcp_servers(&mut servers);
    let skill_order = skills
        .iter()
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let mcp_order = servers
        .iter()
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    for item in &mut skills {
        item.enabled = !item.enabled;
    }
    for item in &mut servers {
        item.enabled = !item.enabled;
    }
    sort_managed_skills(&mut skills);
    sort_managed_mcp_servers(&mut servers);

    assert_eq!(
        skills
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>(),
        skill_order
    );
    assert_eq!(
        servers
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>(),
        mcp_order
    );
}

#[test]
fn managed_agents_block_preserves_user_content_and_replaces_only_managed_block() {
    let codex_dir = temp_codex_dir("managed-agents");
    let original = "# 我自己的规则\n使用 pnpm。\n";
    write_text(&agents_path(&codex_dir), original).expect("write original agents");

    install_managed_agents_block(
        &codex_dir,
        "builtin:first",
        "# First managed prompt\nfirst rule",
    )
    .expect("install first block");
    install_managed_agents_block(
        &codex_dir,
        "builtin:second",
        "# Second managed prompt\nsecond rule",
    )
    .expect("replace managed block");

    let installed = fs::read_to_string(agents_path(&codex_dir)).expect("read agents");
    assert!(installed.starts_with(original.trim_end()));
    assert!(installed.contains("# Second managed prompt"));
    assert!(!installed.contains("# First managed prompt"));
    assert_eq!(installed.matches(AGENTS_MANAGED_BEGIN).count(), 1);
    assert_eq!(
        managed_agents_template_key_from_content(&installed).as_deref(),
        Some("builtin:second")
    );

    assert!(uninstall_managed_agents_block(&codex_dir).expect("uninstall block"));
    assert_eq!(
        fs::read_to_string(agents_path(&codex_dir)).expect("read restored agents"),
        original
    );
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn managed_agents_block_rejects_incomplete_markers_without_writing() {
    let codex_dir = temp_codex_dir("managed-agents-incomplete");
    let broken = format!("# user\n\n{AGENTS_MANAGED_BEGIN}\nunfinished\n");
    write_text(&agents_path(&codex_dir), &broken).expect("write broken agents");

    let result = install_managed_agents_block(&codex_dir, "builtin:test", "content");
    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(agents_path(&codex_dir)).expect("read unchanged agents"),
        broken
    );
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn github_catalog_discovers_new_markdown_without_a_hardcoded_id() {
    let catalog = github_prompt_catalog_from_entries(vec![
            GithubContentEntry {
                name: "brand-new-prompt.md".to_string(),
                kind: "file".to_string(),
                download_url: Some(
                    "https://raw.githubusercontent.com/yynxxxxx/Codex-X/main/examples/brand-new-prompt.md"
                        .to_string(),
                ),
            },
            GithubContentEntry {
                name: "notes.txt".to_string(),
                kind: "file".to_string(),
                download_url: Some("https://example.invalid/notes.txt".to_string()),
            },
            GithubContentEntry {
                name: "BRAND-NEW-PROMPT.MD".to_string(),
                kind: "file".to_string(),
                download_url: Some(
                    "https://raw.githubusercontent.com/yynxxxxx/Codex-X/main/examples/BRAND-NEW-PROMPT.MD"
                        .to_string(),
                ),
            },
        ])
        .expect("build GitHub prompt catalog");
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].1, "brand-new-prompt.md");
    assert!(catalog[0].0.starts_with("github-brand-new-prompt-"));
    assert_eq!(
        stable_remote_prompt_id("brand-new-prompt.md"),
        stable_remote_prompt_id("BRAND-NEW-PROMPT.MD")
    );
}

#[test]
fn github_catalog_rejects_markdown_without_a_download_url() {
    let catalog = github_prompt_catalog_from_entries(vec![GithubContentEntry {
        name: "missing-url.md".to_string(),
        kind: "file".to_string(),
        download_url: None,
    }]);

    assert!(catalog.is_err());
}

#[test]
fn jsdelivr_catalog_keeps_only_direct_markdown_files() {
    let catalog = jsdelivr_prompt_catalog_from_entries(vec![
        "/examples/new prompt.md".to_string(),
        "/examples/NEW PROMPT.MD".to_string(),
        "/examples/海鸥模板.md".to_string(),
        "/examples/nested/ignored.md".to_string(),
        "/examples/notes.txt".to_string(),
        "/docs/ignored.md".to_string(),
    ])
    .expect("build jsDelivr prompt catalog");

    assert_eq!(catalog.len(), 2);
    assert!(catalog
        .iter()
        .any(|(_, filename)| filename == "new prompt.md"));
    assert!(catalog
        .iter()
        .any(|(_, filename)| filename == "海鸥模板.md"));
}

#[test]
fn jsdelivr_catalog_rejects_an_empty_markdown_listing() {
    let catalog = jsdelivr_prompt_catalog_from_entries(vec![
        "/examples/readme.txt".to_string(),
        "/examples/nested/prompt.md".to_string(),
    ]);

    assert!(catalog.is_err());
}

#[test]
fn prompt_download_sources_are_cdn_first_and_encode_the_filename() {
    let sources = prompt_content_source_urls("模板 1#%.md");
    let encoded = "%E6%A8%A1%E6%9D%BF%201%23%25%2Emd";

    assert_eq!(sources.len(), 2);
    assert_eq!(
        sources[0],
        format!("https://cdn.jsdelivr.net/gh/yynxxxxx/Codex-X@main/examples/{encoded}")
    );
    assert_eq!(
        sources[1],
        format!("https://raw.githubusercontent.com/yynxxxxx/Codex-X/main/examples/{encoded}")
    );
}

#[test]
fn empty_cache_fallback_uses_only_bundled_prompts() {
    let statuses = cached_prompt_fallback_statuses(Vec::new());
    let ids = statuses
        .iter()
        .map(|status| status.id.as_str())
        .collect::<HashSet<_>>();

    assert_eq!(statuses.len(), bundled_prompt_metas().len());
    assert_eq!(ids.len(), statuses.len());
    assert!(statuses
        .iter()
        .all(|status| status.content_source == "bundled"
            && !status.cached
            && status.sync_issue.is_none()));
    let sol = statuses
        .iter()
        .find(|status| status.filename == "gpt-5.6-sol-unrestricted.md")
        .expect("gpt-5.6 SOL is bundled");
    assert_eq!(sol.id, "github-gpt-5-6-sol-unrestricted-33b86c71");
    assert_eq!(sol.subtitle, "gpt5.6-sol 破甲提示词");
    let seagull = statuses
        .iter()
        .find(|status| status.filename == "海鸥3.0破甲.md")
        .expect("Seagull 3.0 is bundled");
    assert_eq!(seagull.id, "github-3-0-b459e1e8");
    assert_eq!(
        stable_remote_prompt_id(&sol.filename),
        "github-gpt-5-6-sol-unrestricted-33b86c71"
    );
    assert_eq!(
        stable_remote_prompt_id(&seagull.filename),
        "github-3-0-b459e1e8"
    );
}

#[test]
fn stale_prompt_cache_ids_follow_authoritative_catalog() {
    let cache = |id: &str, filename: &str| CachedBuiltinPrompt {
        id: id.to_string(),
        filename: filename.to_string(),
        source_url: format!("https://example.invalid/{filename}"),
        content: "cached".to_string(),
        checked_at: "2026-07-11T00:00:00+08:00".to_string(),
    };
    let caches = vec![
        cache("gpt5.5-unrestricted", "gpt5.5-unrestricted.md"),
        cache("github-new", "new.md"),
        cache("github-removed", "removed.md"),
        cache("legacy-alias", "new.md"),
    ];
    let active_ids = HashSet::from(["gpt5.5-unrestricted".to_string(), "github-new".to_string()]);

    assert_eq!(
        stale_cached_prompt_ids(&caches, &active_ids),
        vec!["github-removed".to_string(), "legacy-alias".to_string()]
    );
}

#[test]
fn cache_fallback_is_unique_and_keeps_remote_templates_offline() {
    let cache = |id: &str, filename: &str| CachedBuiltinPrompt {
        id: id.to_string(),
        filename: filename.to_string(),
        source_url: format!("https://example.invalid/{filename}"),
        content: "cached".to_string(),
        checked_at: "2026-07-11T00:00:00+08:00".to_string(),
    };
    let statuses = cached_prompt_fallback_statuses(vec![
        cache("gpt5.5-unrestricted", "gpt5.5-unrestricted.md"),
        cache("gpt5.4-unrestricted", "gpt5.4-unrestricted.md"),
        cache("gpt5.5-jeli", "gpt5.5-jeli.md"),
        cache(
            "github-gpt-5-6-sol-unrestricted-33b86c71",
            "gpt-5.6-sol-unrestricted.md",
        ),
        cache("github-3-0-b459e1e8", "海鸥3.0破甲.md"),
        cache("github-new", "new.md"),
        cache("legacy-new", "new.md"),
    ]);
    let ids = statuses
        .iter()
        .map(|status| status.id.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let filenames = statuses
        .iter()
        .map(|status| status.filename.to_ascii_lowercase())
        .collect::<HashSet<_>>();

    assert_eq!(statuses.len(), bundled_prompt_metas().len() + 1);
    assert_eq!(ids.len(), statuses.len());
    assert_eq!(filenames.len(), statuses.len());
    assert!(statuses.iter().any(|status| status.filename == "new.md"));
}

#[test]
fn deleting_stale_prompt_cache_ids_removes_database_rows() {
    let mut conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
            "CREATE TABLE builtin_prompt_cache (id TEXT PRIMARY KEY);
             INSERT INTO builtin_prompt_cache (id) VALUES ('keep'), ('remove-old'), ('remove-alias');",
        )
        .expect("seed prompt cache");
    let stale_ids = vec!["remove-old".to_string(), "remove-alias".to_string()];

    assert_eq!(
        delete_cached_prompt_ids(&mut conn, &stale_ids).expect("delete stale rows"),
        2
    );
    let remaining = conn
        .query_row(
            "SELECT group_concat(id, ',') FROM builtin_prompt_cache",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("read remaining rows");
    assert_eq!(remaining, "keep");
}

#[test]
fn full_toml_match_selects_only_the_actual_provider() {
    let first_toml = r#"model_provider = "custom"
model = "gpt-5.5"
model_reasoning_effort = "high"

[model_providers.custom]
name = "Same API"
base_url = "https://example.com/v1"
wire_api = "responses"
"#;
    let second_toml = r#"model_provider = "custom"
model = "gpt-5.5"
model_reasoning_effort = "xhigh"

[model_providers.custom]
name = "Same API"
base_url = "https://example.com/v1"
wire_api = "responses"
"#;
    let provider = |id: &str, toml: &str| SavedProvider {
        id: id.to_string(),
        provider_name: "Same API".to_string(),
        base_url: "https://example.com/v1".to_string(),
        model: "gpt-5.5".to_string(),
        api_key: Some("sk-same".to_string()),
        toml_config: Some(toml.to_string()),
        wire_api: "responses".to_string(),
        requires_openai_auth: true,
    };
    let live = second_toml.replace(
        "wire_api = \"responses\"",
        "wire_api = \"responses\"\nexperimental_bearer_token = \"sk-same\"",
    );
    let matched = active_saved_provider_id_from_config(
        &live,
        &[
            provider("first", first_toml),
            provider("second", second_toml),
        ],
    );
    assert_eq!(matched.as_deref(), Some("second"));
}

#[test]
fn append_mode_preserves_external_prompt_and_disable_removes_only_managed_agents() {
    let codex_dir = temp_codex_dir("追加-prompt");
    write_text(
        &config_path(&codex_dir),
        "model = \"gpt-5.5\"\nmodel_instructions_file = \"./user-original.md\"\n",
    )
    .expect("write config");
    write_text(&codex_dir.join("user-original.md"), "user prompt").expect("write user prompt");
    write_text(&agents_path(&codex_dir), "# User AGENTS\nkeep this\n").expect("write agents");

    let enabled = enable_prompt_content_inner(
        Some(codex_dir.display().to_string()),
        INSTRUCTION_FILENAME,
        "managed prompt",
        "builtin:gpt5.5-unrestricted",
        "managed",
        "test",
        PromptInjectionMode::Append,
        "test-append",
    )
    .expect("enable append");
    assert_eq!(
        enabled.state.instruction_injection_mode.as_deref(),
        Some("append")
    );
    assert!(enabled.state.instruction_enabled);
    let config = fs::read_to_string(config_path(&codex_dir)).expect("read config");
    assert!(config.contains("model_instructions_file = \"./user-original.md\""));
    let agents = fs::read_to_string(agents_path(&codex_dir)).expect("read agents");
    assert!(agents.contains("# User AGENTS"));
    assert!(agents.contains("managed prompt"));
    enable_prompt_content_inner(
        Some(codex_dir.display().to_string()),
        INSTRUCTION_FILENAME,
        "managed prompt",
        "builtin:gpt5.5-unrestricted",
        "managed",
        "test",
        PromptInjectionMode::Append,
        "test-append-again",
    )
    .expect("enable append again");
    let agents = fs::read_to_string(agents_path(&codex_dir)).expect("read repeated agents");
    assert_eq!(agents.matches(AGENTS_MANAGED_BEGIN).count(), 1);

    disable_instruction_inner(Some(codex_dir.display().to_string()), Some(true))
        .expect("disable managed append");
    let config = fs::read_to_string(config_path(&codex_dir)).expect("read config after disable");
    assert!(config.contains("model_instructions_file = \"./user-original.md\""));
    assert_eq!(
        fs::read_to_string(agents_path(&codex_dir)).expect("read agents after disable"),
        "# User AGENTS\nkeep this\n"
    );
    assert!(codex_dir.join("user-original.md").exists());
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn replace_mode_keeps_unrelated_agents_content() {
    let codex_dir = temp_codex_dir("替换-prompt");
    write_text(&agents_path(&codex_dir), "# User AGENTS\nkeep this\n").expect("write agents");

    let enabled = enable_prompt_content_inner(
        Some(codex_dir.display().to_string()),
        INSTRUCTION_FILENAME,
        "managed prompt",
        "builtin:gpt5.5-unrestricted",
        "managed",
        "test",
        PromptInjectionMode::Replace,
        "test-replace",
    )
    .expect("enable replace");
    assert_eq!(
        enabled.state.instruction_injection_mode.as_deref(),
        Some("replace")
    );
    assert_eq!(
        fs::read_to_string(agents_path(&codex_dir)).expect("read agents"),
        "# User AGENTS\nkeep this\n"
    );
    assert!(fs::read_to_string(config_path(&codex_dir))
        .expect("read config")
        .contains("model_instructions_file = \"./gpt5.5-unrestricted.md\""));
    enable_prompt_content_inner(
        Some(codex_dir.display().to_string()),
        INSTRUCTION_FILENAME,
        "updated managed prompt",
        "builtin:gpt5.5-unrestricted",
        "managed",
        "test",
        PromptInjectionMode::Replace,
        "test-replace-again",
    )
    .expect("enable replace again");
    assert_eq!(
        fs::read_to_string(codex_dir.join(INSTRUCTION_FILENAME)).expect("read replaced prompt"),
        "updated managed prompt"
    );
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn restore_backup_restores_agents_file_alongside_config() {
    let codex_dir = temp_codex_dir("restore-agents");
    write_text(&config_path(&codex_dir), "model = \"gpt-5.5\"\n").expect("write config");
    write_text(&agents_path(&codex_dir), "# Original AGENTS\n").expect("write agents");
    let backup_id = create_backup(&codex_dir, "before-agents-change")
        .expect("create backup")
        .expect("backup id");

    write_text(&config_path(&codex_dir), "model = \"changed\"\n").expect("change config");
    write_text(&agents_path(&codex_dir), "# Changed AGENTS\n").expect("change agents");
    restore_backup_inner(Some(codex_dir.display().to_string()), backup_id).expect("restore backup");

    assert_eq!(
        fs::read_to_string(config_path(&codex_dir)).expect("read restored config"),
        "model = \"gpt-5.5\"\n"
    );
    assert_eq!(
        fs::read_to_string(agents_path(&codex_dir)).expect("read restored agents"),
        "# Original AGENTS\n"
    );
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn skill_metadata_reads_frontmatter_name_before_directory() {
    let dir = temp_codex_dir("skill-frontmatter").join("skill-zip-123");
    fs::create_dir_all(&dir).expect("create skill dir");
    write_text(
        &dir.join("SKILL.md"),
        r#"---
name: ctf-sandbox-runner
description: Resume authorized CTF sandbox projects.
---

# CTF Sandbox Runner
"#,
    )
    .expect("write skill");

    let (name, desc) = read_skill_metadata(&dir, "skill-zip-123");
    assert_eq!(name, "ctf-sandbox-runner");
    assert_eq!(
        desc.as_deref(),
        Some("Resume authorized CTF sandbox projects.")
    );

    let root = dir.parent().unwrap().to_path_buf();
    let _ = fs::remove_dir_all(root);
}

#[test]
fn normalize_legacy_zip_skill_dir_renames_to_metadata_name() {
    let root = temp_codex_dir("skill-normalize");
    let dir = root.join("skill-zip-1783334291187");
    fs::create_dir_all(&dir).expect("create legacy skill dir");
    write_text(
        &dir.join("SKILL.md"),
        r#"---
name: mission-keeper
description: Keep long investigations aligned.
---
"#,
    )
    .expect("write skill");

    normalize_legacy_zip_skill_dirs(&root).expect("normalize");
    assert!(!dir.exists());
    assert!(root.join("mission-keeper").join("SKILL.md").exists());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn switch_provider_writes_scoped_bearer_and_api_key_auth_mode() {
    let codex_dir = temp_codex_dir("switch-provider");
    let result = switch_provider_inner(ProviderInput {
        config_dir: Some(codex_dir.display().to_string()),
        _provider_id: Some("magicai".to_string()),
        provider_name: "MagicAI".to_string(),
        base_url: "https://example.com/v1/".to_string(),
        model: "gpt-5.5".to_string(),
        api_key: Some("sk-test".to_string()),
        wire_api: Some("responses".to_string()),
        requires_openai_auth: None,
    })
    .expect("switch provider");

    assert_eq!(result.state.model_provider.as_deref(), Some("custom"));
    assert_eq!(result.state.model.as_deref(), Some("gpt-5.5"));

    let config_text = fs::read_to_string(config_path(&codex_dir)).expect("read config");
    assert!(config_text.contains("model_provider = \"custom\""));
    assert!(config_text.contains("[model_providers.custom]"));
    assert!(config_text.contains("name = \"MagicAI\""));
    assert!(config_text.contains("base_url = \"https://example.com/v1\""));
    assert!(config_text.contains("requires_openai_auth = true"));
    let config_doc = config_text
        .parse::<DocumentMut>()
        .expect("parse switched config");
    assert_eq!(
        config_doc["model_providers"]["custom"]["experimental_bearer_token"].as_str(),
        Some("sk-test")
    );
    assert!(config_doc.get("experimental_bearer_token").is_none());

    let auth_text = fs::read_to_string(auth_path(&codex_dir)).expect("read auth");
    let auth: Value = serde_json::from_str(&auth_text).expect("parse auth");
    assert_eq!(
        auth.get("OPENAI_API_KEY").and_then(Value::as_str),
        Some("sk-test")
    );
    assert_eq!(
        auth.get("auth_mode").and_then(Value::as_str),
        Some("apikey")
    );

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn switch_provider_persists_detected_custom_before_overwrite() {
    let codex_dir = temp_codex_dir("switch-provider-persist-current");
    write_text(
        &config_path(&codex_dir),
        r#"model_provider = "custom"
model = "model-a"

[model_providers.custom]
name = "Provider A"
base_url = "https://a.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-a-scoped"
"#,
    )
    .expect("write provider A config");
    write_text(&auth_path(&codex_dir), "{ invalid auth").expect("write malformed provider A auth");

    let persisted = std::cell::RefCell::new(None);
    let result = switch_provider_with_pre_persist(
        ProviderInput {
            config_dir: Some(codex_dir.display().to_string()),
            _provider_id: Some("provider-b".to_string()),
            provider_name: "Provider B".to_string(),
            base_url: "https://b.example.com/v1".to_string(),
            model: "model-b".to_string(),
            api_key: Some("sk-b".to_string()),
            wire_api: Some("responses".to_string()),
            requires_openai_auth: Some(false),
        },
        |dir| {
            *persisted.borrow_mut() = detected_live_custom_provider(dir)?;
            Ok(())
        },
    )
    .expect("switch to provider B");

    let provider_a = persisted.into_inner().expect("provider A persisted");
    assert_eq!(provider_a.provider_name, "Provider A");
    assert_eq!(provider_a.base_url, "https://a.example.com/v1");
    assert_eq!(provider_a.model, "model-a");
    assert_eq!(provider_a.api_key.as_deref(), Some("sk-a-scoped"));
    assert!(!provider_a
        .toml_config
        .as_deref()
        .unwrap_or_default()
        .contains("experimental_bearer_token"));
    assert_eq!(result.state.model.as_deref(), Some("model-b"));
    assert!(result
        .state
        .config_text
        .contains("https://b.example.com/v1"));
    assert!(!result
        .state
        .config_text
        .contains("https://a.example.com/v1"));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn switch_provider_reserved_builtin_ids_still_write_live_custom() {
    let codex_dir = temp_codex_dir("switch-provider-reserved");
    let result = switch_provider_inner(ProviderInput {
        config_dir: Some(codex_dir.display().to_string()),
        _provider_id: Some("openai".to_string()),
        provider_name: "OpenAI".to_string(),
        base_url: "https://proxy.example.com/v1".to_string(),
        model: "gpt-5.5".to_string(),
        api_key: Some("sk-proxy".to_string()),
        wire_api: Some("responses".to_string()),
        requires_openai_auth: None,
    })
    .expect("switch provider");

    assert_eq!(result.state.model_provider.as_deref(), Some("custom"));
    let config_text = fs::read_to_string(config_path(&codex_dir)).expect("read config");
    assert!(config_text.contains("model_provider = \"custom\""));
    assert!(config_text.contains("[model_providers.custom]"));
    assert!(!config_text.contains("[model_providers.openai]"));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn switch_official_persists_detected_custom_before_overwrite() {
    let codex_dir = temp_codex_dir("switch-official-persist-current");
    write_text(
        &config_path(&codex_dir),
        r#"model_provider = "custom"
model = "model-a"

[model_providers.custom]
name = "Provider A"
base_url = "https://a.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#,
    )
    .expect("write provider A config");
    write_json(
        &auth_path(&codex_dir),
        &json!({"OPENAI_API_KEY": "sk-a-auth", "auth_mode": "apikey"}),
    )
    .expect("write provider A auth");

    let persisted = std::cell::RefCell::new(None);
    let result =
        switch_official_provider_with_pre_persist(Some(codex_dir.display().to_string()), |dir| {
            *persisted.borrow_mut() = detected_live_custom_provider(dir)?;
            Ok(())
        })
        .expect("switch to official");

    let provider_a = persisted.into_inner().expect("provider A persisted");
    assert_eq!(provider_a.provider_name, "Provider A");
    assert_eq!(provider_a.base_url, "https://a.example.com/v1");
    assert_eq!(provider_a.api_key.as_deref(), Some("sk-a-auth"));
    assert_eq!(result.state.model_provider.as_deref(), Some("openai"));
    assert!(!result
        .state
        .config_text
        .contains("https://a.example.com/v1"));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn switch_official_preserves_live_auth_json() {
    let codex_dir = temp_codex_dir("switch-official-preserve-auth");
    write_text(
        &config_path(&codex_dir),
        r#"model_provider = "custom"
model = "gpt-5.5"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#,
    )
    .expect("write config");
    write_json(
        &auth_path(&codex_dir),
        &json!({
            "OPENAI_API_KEY": "sk-live",
            "auth_mode": "apikey"
        }),
    )
    .expect("write auth");

    let result =
        switch_official_provider_inner(Some(codex_dir.display().to_string())).expect("switch");
    assert_eq!(result.state.model_provider.as_deref(), Some("openai"));

    let auth_text = fs::read_to_string(auth_path(&codex_dir)).expect("read auth");
    let auth: Value = serde_json::from_str(&auth_text).expect("parse auth");
    assert_eq!(
        auth.get("OPENAI_API_KEY").and_then(Value::as_str),
        Some("sk-live")
    );
    assert_eq!(
        auth.get("auth_mode").and_then(Value::as_str),
        Some("apikey")
    );

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn provider_status_403_is_not_ok() {
    let result = provider_status_result(403, 123);
    assert!(!result.ok);
    assert_eq!(result.status, Some(403));
    assert_eq!(result.duration_ms, 123);
}

#[test]
fn import_ccswitch_provider_reads_experimental_bearer_token() {
    let settings_config = json!({
        "auth": {},
        "config": r#"model_provider = "custom"
model = "gpt-5.5"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-from-config"
"#,
    })
    .to_string();

    let row = CcSwitchCodexRow {
        id: "openai".to_string(),
        name: "Proxy".to_string(),
        settings_config,
        category: None,
    };
    let provider = build_ccswitch_codex_provider(&row, &HashMap::new()).expect("provider");
    assert_eq!(provider.id, "openai-custom");
    assert_eq!(provider.api_key.as_deref(), Some("sk-from-config"));
    assert_eq!(provider.base_url, "https://proxy.example.com/v1");
}

#[test]
fn import_ccswitch_provider_uses_row_id_section_not_stale_active_provider() {
    let sky_row = CcSwitchCodexRow {
        id: "sky2api-1782194988817".to_string(),
        name: "Sky2api".to_string(),
        settings_config: json!({
            "auth": {"OPENAI_API_KEY": "sk-sky"},
            "config": r#"model = "gpt-5.5"
model_provider = "magicai-1782956845071"

[model_providers.magicai-1782956845071]
name = "MagicAI"
base_url = "https://sky1818.com"
wire_api = "responses"
requires_openai_auth = true
"#,
        })
        .to_string(),
        category: None,
    };
    let magic_row = CcSwitchCodexRow {
        id: "magicai-1782956845071".to_string(),
        name: "MagicAI".to_string(),
        settings_config: json!({
            "auth": {"OPENAI_API_KEY": "sk-magic"},
            "config": r#"model = "gpt-5.5"
model_provider = "sky2api-1782194988817"

[model_providers.magicai-1782956845071]
name = "MagicAI"
base_url = "https://sky1818.com"
wire_api = "responses"
requires_openai_auth = true

[model_providers.sky2api-1782194988817]
name = "Sky2api"
base_url = "https://ikuncode.site/v1"
wire_api = "responses"
requires_openai_auth = true
"#,
        })
        .to_string(),
        category: None,
    };

    let mut sections = HashMap::new();
    for row in [&sky_row, &magic_row] {
        let settings: Value = serde_json::from_str(&row.settings_config).expect("settings");
        for section in
            codex_sections_from_config(settings.get("config").and_then(Value::as_str).unwrap_or(""))
        {
            sections.entry(section.id.clone()).or_insert(section);
        }
    }

    let sky = build_ccswitch_codex_provider(&sky_row, &sections).expect("sky");
    let magic = build_ccswitch_codex_provider(&magic_row, &sections).expect("magic");

    assert_eq!(sky.provider_name, "Sky2api");
    assert_eq!(sky.base_url, "https://ikuncode.site/v1");
    assert_eq!(sky.api_key.as_deref(), Some("sk-sky"));

    assert_eq!(magic.provider_name, "MagicAI");
    assert_eq!(magic.base_url, "https://sky1818.com");
    assert_eq!(magic.api_key.as_deref(), Some("sk-magic"));
}

#[test]
fn save_provider_toml_config_writes_provider_scoped_bearer_token() {
    let codex_dir = temp_codex_dir("save-provider-toml-token");
    let result = save_provider_toml_config_inner(ProviderTomlInput {
        config_dir: Some(codex_dir.display().to_string()),
        config_text: r#"model_provider = "proxy"
model = "gpt-5.5"

[model_providers.proxy]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
"#
        .to_string(),
        api_key: Some("sk-provider-table".to_string()),
    })
    .expect("save provider toml");

    assert!(result.ok);
    let config_text = fs::read_to_string(config_path(&codex_dir)).expect("read config");
    assert!(config_text.contains("[model_providers.proxy]"));
    assert!(config_text.contains("experimental_bearer_token = \"sk-provider-table\""));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn save_provider_toml_persists_detected_custom_before_overwrite() {
    let codex_dir = temp_codex_dir("save-provider-toml-persist-current");
    write_text(
        &config_path(&codex_dir),
        r#"model_provider = "custom"
model = "model-a"

[model_providers.custom]
name = "Provider A"
base_url = "https://a.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
experimental_bearer_token = "sk-a"
"#,
    )
    .expect("write provider A config");

    let persisted = std::cell::RefCell::new(None);
    let result = save_provider_toml_config_with_pre_persist(
        ProviderTomlInput {
            config_dir: Some(codex_dir.display().to_string()),
            config_text: r#"model_provider = "custom"
model = "model-b"

[model_providers.custom]
name = "Provider B"
base_url = "https://b.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
"#
            .to_string(),
            api_key: Some("sk-b".to_string()),
        },
        |dir| {
            *persisted.borrow_mut() = detected_live_custom_provider(dir)?;
            Ok(())
        },
    )
    .expect("save provider B toml");

    let provider_a = persisted.into_inner().expect("provider A persisted");
    assert_eq!(provider_a.provider_name, "Provider A");
    assert_eq!(provider_a.base_url, "https://a.example.com/v1");
    assert_eq!(provider_a.api_key.as_deref(), Some("sk-a"));
    assert_eq!(result.state.model.as_deref(), Some("model-b"));
    assert!(result
        .state
        .config_text
        .contains("https://b.example.com/v1"));
    assert!(result
        .state
        .config_text
        .contains("experimental_bearer_token = \"sk-b\""));
    assert!(!result
        .state
        .config_text
        .contains("https://a.example.com/v1"));

    let _ = fs::remove_dir_all(codex_dir);
}

fn seed_thread_database(path: &Path, sessions: &[(&str, &Path)], spawn_edge: Option<(&str, &str)>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create sqlite parent");
    }
    let conn = Connection::open(path).expect("open thread database");
    conn.execute_batch(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                model_provider TEXT NOT NULL,
                rollout_path TEXT
             );
             CREATE TABLE thread_dynamic_tools (thread_id TEXT NOT NULL);
             CREATE TABLE thread_spawn_edges (parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL);
             CREATE TABLE agent_job_items (assigned_thread_id TEXT);",
        )
        .expect("create thread schema");
    for (id, rollout) in sessions {
        conn.execute(
            "INSERT INTO threads (id, model_provider, rollout_path) VALUES (?1, 'openai', ?2)",
            (id, rollout.display().to_string()),
        )
        .expect("insert thread");
        conn.execute(
            "INSERT INTO thread_dynamic_tools (thread_id) VALUES (?1)",
            [id],
        )
        .expect("insert dynamic tool");
        conn.execute(
            "INSERT INTO agent_job_items (assigned_thread_id) VALUES (?1)",
            [id],
        )
        .expect("insert job item");
    }
    if let Some((parent, child)) = spawn_edge {
        conn.execute(
            "INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id) VALUES (?1, ?2)",
            (parent, child),
        )
        .expect("insert spawn edge");
    }
}

fn sqlite_count(path: &Path, sql: &str) -> i64 {
    Connection::open(path)
        .expect("open sqlite for count")
        .query_row(sql, [], |row| row.get(0))
        .expect("read sqlite count")
}

fn write_rollout_fixture(
    path: &Path,
    thread_id: &str,
    provider: Option<&str>,
    response_items: &str,
) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create rollout parent");
    }
    let provider = provider
        .map(|value| format!(",\"model_provider\":\"{value}\""))
        .unwrap_or_default();
    let content = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{thread_id}\"{provider},\"cwd\":\"/tmp/project\"}}}}\n{response_items}"
        );
    write_text(path, &content).expect("write rollout fixture");
}

fn thread_provider(path: &Path, id: &str) -> String {
    Connection::open(path)
        .expect("open sqlite for provider")
        .query_row(
            "SELECT model_provider FROM threads WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .expect("read thread provider")
}

#[test]
fn provider_sync_rewrites_every_session_meta_and_preserves_item_ids() {
    let codex_dir = temp_codex_dir("target-provider-all-meta");
    let database = codex_dir.join("state_5.sqlite");
    let thread_id = "019f6000-0000-7000-8000-000000000101";
    let child_id = "019f6000-0000-7000-8000-000000000102";
    let rollout = codex_dir.join("sessions/rollout-mixed-meta.jsonl");
    let content = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{thread_id}\",\"model_provider\":\"openai\",\"cwd\":\"/tmp/project\"}}}}\n\
             {{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{thread_id}\",\"model_provider\":\"custom\",\"cwd\":\"/tmp/project\"}}}}\n\
             {{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{child_id}\",\"cwd\":\"/tmp/child\"}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"id\":\"item_40040926a4b5daaa9118466b\",\"role\":\"assistant\",\"content\":[]}}}}\n"
        );
    write_text(&rollout, &content).expect("write mixed rollout");
    seed_thread_database(&database, &[(thread_id, &rollout)], None);

    let status = session_sync_status_inner(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
    )
    .expect("scan mixed providers");
    assert!(status.needs_sync);
    assert_eq!(status.mismatched_rollouts, 1);
    assert_eq!(status.mismatched_session_meta, 2);
    assert!(status.warnings.is_empty());

    let result = sync_sessions_provider_inner(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
    )
    .expect("sync every session meta");
    assert_eq!(result.updated_rollouts, 1);
    assert_eq!(thread_provider(&database, thread_id), "custom");

    let repaired = fs::read_to_string(&rollout).expect("read repaired rollout");
    assert!(repaired.contains("item_40040926a4b5daaa9118466b"));
    let providers = repaired
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record.get("type").and_then(Value::as_str) == Some("session_meta"))
        .filter_map(|record| {
            record
                .get("payload")
                .and_then(Value::as_object)
                .and_then(|payload| payload.get("model_provider"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect::<Vec<_>>();
    assert_eq!(providers, vec!["custom", "custom", "custom"]);
    assert!(!result.status.needs_sync);

    let metadata = fs::read_to_string(PathBuf::from(&result.backup_dir).join("metadata.json"))
        .expect("read backup metadata");
    assert!(metadata.contains("\"managedBy\": \"Codex-X provider sync v2\""));

    let second = sync_sessions_provider_inner(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
    )
    .expect("second sync is a no-op");
    assert_eq!(second.updated_rollouts, 0);
    assert_eq!(second.updated_threads, 0);
    assert!(second.backup_dir.is_empty());
    assert!(second.status.warnings.is_empty());

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn user_event_flag_does_not_make_sessions_need_provider_sync() {
    let codex_dir = temp_codex_dir("user-event-flag-is-derived");
    let parent_id = "019f6000-0000-7000-8000-000000000109";
    let child_id = "019f6000-0000-7000-8000-000000000110";
    let parent_rollout = codex_dir.join("sessions/rollout-parent-user-event.jsonl");
    let child_rollout = codex_dir.join("sessions/rollout-child-user-event.jsonl");
    for (path, id) in [(&parent_rollout, parent_id), (&child_rollout, child_id)] {
        write_rollout_fixture(
            path,
            id,
            Some("custom"),
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"hello\"}}\n",
        );
    }
    let database = codex_dir.join("state_5.sqlite");
    seed_thread_database(
        &database,
        &[(parent_id, &parent_rollout), (child_id, &child_rollout)],
        Some((parent_id, child_id)),
    );
    Connection::open(&database)
        .expect("open session database")
        .execute_batch(
            "ALTER TABLE threads ADD COLUMN has_user_event INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE threads ADD COLUMN cwd TEXT;
             ALTER TABLE threads ADD COLUMN preview TEXT;
             UPDATE threads
             SET model_provider = 'custom', cwd = '/tmp/project', preview = 'visible';",
        )
        .expect("seed derived user event flags");

    let status = session_sync_status_inner(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
    )
    .expect("scan matching sessions");
    assert_eq!(status.top_level_threads, 1);
    assert_eq!(status.subagent_threads, 1);
    assert_eq!(status.mismatched_rollouts, 0);
    assert_eq!(status.mismatched_threads, 0);
    assert!(!status.needs_sync);
    assert!(status.sessions.iter().all(|session| !session.needs_sync));

    let result = sync_sessions_provider_inner(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
    )
    .expect("matching sessions are a no-op");
    assert_eq!(result.updated_rollouts, 0);
    assert_eq!(result.updated_threads, 0);
    assert!(result.backup_dir.is_empty());
    assert_eq!(
        sqlite_count(&database, "SELECT SUM(has_user_event) FROM threads"),
        0
    );

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn provider_sync_updates_provider_and_cwd_without_touching_user_flag() {
    let codex_dir = temp_codex_dir("target-provider-all-dbs");
    let thread_id = "019f6000-0000-7000-8000-000000000111";
    let rollout = codex_dir.join("sessions/rollout-metadata.jsonl");
    write_rollout_fixture(
        &rollout,
        thread_id,
        Some("openai"),
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"hello\"}}\n",
    );
    let databases = [
        codex_dir.join("sqlite/state_5.sqlite"),
        codex_dir.join("state_5.sqlite"),
    ];
    for database in &databases {
        seed_thread_database(database, &[(thread_id, &rollout)], None);
        let conn = Connection::open(database).expect("open sqlite");
        conn.execute_batch(
            "ALTER TABLE threads ADD COLUMN has_user_event INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE threads ADD COLUMN cwd TEXT;
                 UPDATE threads SET cwd = '/tmp/wrong';",
        )
        .expect("seed index drift");
    }

    assert_eq!(sqlite_session_db_paths(&codex_dir), databases);
    let status = session_sync_status_inner(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
    )
    .expect("scan duplicate database rows");
    assert_eq!(status.sqlite_threads, 1);
    assert_eq!(status.mismatched_threads, 1);
    assert_eq!(status.sessions.len(), 1);
    let result = sync_sessions_provider_inner(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
    )
    .expect("sync all databases");
    assert_eq!(result.updated_rollouts, 1);
    assert_eq!(result.updated_threads, 4);
    for database in &databases {
        let repaired = Connection::open(database)
            .expect("open repaired sqlite")
            .query_row(
                "SELECT model_provider, has_user_event, cwd FROM threads WHERE id = ?1",
                [thread_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .expect("read repaired metadata");
        assert_eq!(
            repaired,
            ("custom".to_string(), 0, "/tmp/project".to_string())
        );
    }

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn changed_rollout_is_skipped_instead_of_overwritten() {
    let codex_dir = temp_codex_dir("provider-sync-changed-rollout");
    let thread_id = "019f6000-0000-7000-8000-000000000115";
    let rollout = codex_dir.join("sessions/rollout-changed.jsonl");
    write_rollout_fixture(&rollout, thread_id, Some("openai"), "");
    let scan = scan_rollouts(&codex_dir, "custom").expect("scan rollout");
    assert_eq!(scan.changes.len(), 1);

    let appended = format!(
        "{}{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\"}}}}\n",
        fs::read_to_string(&rollout).expect("read original rollout")
    );
    write_text(&rollout, &appended).expect("simulate Codex append");
    let (applied, skipped) = apply_session_changes(&scan.changes).expect("guard changed file");
    assert!(applied.is_empty());
    assert_eq!(skipped, vec![rollout.clone()]);
    assert_eq!(
        fs::read_to_string(&rollout).expect("read guarded file"),
        appended
    );

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn rollback_refuses_to_overwrite_a_file_changed_after_apply() {
    let codex_dir = temp_codex_dir("provider-sync-rollback-guard");
    let thread_id = "019f6000-0000-7000-8000-000000000116";
    let rollout = codex_dir.join("sessions/rollout-rollback-guard.jsonl");
    write_rollout_fixture(&rollout, thread_id, Some("openai"), "");
    let scan = scan_rollouts(&codex_dir, "custom").expect("scan rollout");
    let (applied, skipped) = apply_session_changes(&scan.changes).expect("apply rollout");
    assert_eq!(applied.len(), 1);
    assert!(skipped.is_empty());

    let mutation = "Codex appended different content after sync\n";
    write_text(&rollout, mutation).expect("mutate applied rollout");
    let error = restore_session_changes(&applied).expect_err("rollback must refuse mutation");
    assert!(error.to_string().contains("有 1 个会话文件无法安全回滚"));
    assert_eq!(
        fs::read_to_string(&rollout).expect("read preserved mutation"),
        mutation
    );

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn provider_sync_restores_jsonl_when_sqlite_update_fails() {
    let codex_dir = temp_codex_dir("target-provider-rollback");
    let database = codex_dir.join("state_5.sqlite");
    let thread_id = "019f6000-0000-7000-8000-000000000121";
    let rollout = codex_dir.join("sessions/rollout-rollback.jsonl");
    write_rollout_fixture(&rollout, thread_id, Some("openai"), "");
    seed_thread_database(&database, &[(thread_id, &rollout)], None);
    Connection::open(&database)
        .expect("open sqlite")
        .execute_batch(
            "CREATE TRIGGER reject_provider_update
                 BEFORE UPDATE OF model_provider ON threads
                 BEGIN SELECT RAISE(ABORT, 'provider update blocked'); END;",
        )
        .expect("install rejecting trigger");
    let original = fs::read(&rollout).expect("read original rollout");

    let error = sync_sessions_provider_inner(
        Some(codex_dir.display().to_string()),
        Some("custom".to_string()),
    )
    .expect_err("sqlite update must fail");
    assert!(error.to_string().contains("provider update blocked"));
    assert_eq!(
        fs::read(&rollout).expect("read rolled back rollout"),
        original
    );
    assert_eq!(thread_provider(&database, thread_id), "openai");

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn backup_pruning_only_removes_v2_provider_sync_backups() {
    let codex_dir = temp_codex_dir("provider-backup-pruning");
    let root = provider_sync_backup_root(&codex_dir);
    for index in 0..7 {
        let historical = root.join(format!("20260714010{index:02}"));
        fs::create_dir_all(&historical).expect("create historical backup");
        write_json(
            &historical.join("metadata.json"),
            &json!({
                "managedBy": "Codex++ provider sync",
                "targetProvider": "openai"
            }),
        )
        .expect("write historical metadata");

        let v2 = root.join(format!("20260715010{index:02}"));
        fs::create_dir_all(&v2).expect("create v2 backup");
        write_json(
            &v2.join("metadata.json"),
            &json!({
                "managedBy": "Codex-X provider sync v2",
                "targetProvider": "custom"
            }),
        )
        .expect("write v2 metadata");
    }

    prune_provider_sync_backups(&codex_dir).expect("prune v2 backups");
    let dirs = fs::read_dir(&root)
        .expect("read backup root")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        dirs.iter()
            .filter(|name| name.starts_with("20260714"))
            .count(),
        7
    );
    assert_eq!(
        dirs.iter()
            .filter(|name| name.starts_with("20260715"))
            .count(),
        5
    );

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn backup_of_external_sqlite_path_never_writes_to_the_source() {
    let codex_dir = temp_codex_dir("external-sqlite-backup-home");
    let external_dir = temp_codex_dir("external-sqlite-source");
    let source = external_dir.join("state_5.sqlite");
    let backup_dir = codex_dir.join("backups_state/provider-sync/test");
    seed_thread_database(&source, &[], None);
    let writer = Connection::open(&source).expect("open external sqlite writer");
    writer
        .pragma_update(None, "journal_mode", "WAL")
        .expect("enable WAL mode");
    writer
            .execute(
                "INSERT INTO threads (id, model_provider, rollout_path) VALUES ('wal-thread', 'custom', NULL)",
                [],
            )
            .expect("write WAL-only row");
    let before = fs::read(&source).expect("read external sqlite before backup");

    backup_sqlite_to_backup(&codex_dir, &backup_dir, &source)
        .expect("snapshot external sqlite into backup");

    assert!(!before.is_empty());
    assert_eq!(fs::read(&source).expect("reread external sqlite"), before);
    let external_root = backup_dir.join("external");
    let hash_dir = fs::read_dir(&external_root)
        .expect("read external backup root")
        .flatten()
        .next()
        .expect("external backup hash directory")
        .path();
    let copied = hash_dir.join("state_5.sqlite");
    assert!(!fs::read(&copied)
        .expect("read external sqlite backup")
        .is_empty());
    assert_eq!(sqlite_count(&copied, "SELECT COUNT(*) FROM threads"), 1);
    drop(writer);

    let _ = fs::remove_dir_all(codex_dir);
    let _ = fs::remove_dir_all(external_dir);
}

#[test]
fn active_session_database_prefers_current_root_over_legacy_sqlite_copy() {
    let codex_dir = temp_codex_dir("active-session-db");
    let current = codex_dir.join("state_5.sqlite");
    let legacy = codex_dir.join("sqlite/state_5.sqlite");
    seed_thread_database(&current, &[], None);
    seed_thread_database(&legacy, &[], None);

    assert_eq!(sqlite_candidate_paths(&codex_dir), vec![current]);

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn active_session_database_prefers_highest_numeric_state_version() {
    let codex_dir = temp_codex_dir("active-session-db-version");
    let old_current = codex_dir.join("state_4.sqlite");
    let newest_current = codex_dir.join("state_10.sqlite");
    let legacy = codex_dir.join("sqlite/state_99.sqlite");
    seed_thread_database(&old_current, &[], None);
    seed_thread_database(&newest_current, &[], None);
    seed_thread_database(&legacy, &[], None);

    assert_eq!(sqlite_candidate_paths(&codex_dir), vec![newest_current]);

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn active_session_verifier_rejects_missing_predelete_database_paths() {
    let ids = HashSet::from(["019f6000-0000-7000-8000-000000000001".to_string()]);

    assert!(active_session_ids_present(&[], &ids).is_err());
}

#[test]
fn active_session_verifier_checks_the_precaptured_database() {
    let codex_dir = temp_codex_dir("active-session-db-verifier");
    let database = codex_dir.join("state_5.sqlite");
    let present_id = "019f6000-0000-7000-8000-000000000001";
    let absent_id = "019f6000-0000-7000-8000-000000000002";
    let rollout = codex_dir.join("sessions/rollout.jsonl");
    seed_thread_database(&database, &[(present_id, &rollout)], None);
    let ids = HashSet::from([present_id.to_string(), absent_id.to_string()]);

    assert_eq!(
        active_session_ids_present(&[database], &ids).expect("verify active database"),
        HashSet::from([present_id.to_string()])
    );

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn session_previews_return_subagents_with_explicit_marker() {
    let codex_dir = temp_codex_dir("session-preview-subagents");
    let database = codex_dir.join("state_5.sqlite");
    let root_a = "019f6000-0000-7000-8000-000000000001";
    let root_b = "019f6000-0000-7000-8000-000000000002";
    let child = "019f6000-0000-7000-8000-000000000003";
    let orphan_subagent = "019f6000-0000-7000-8000-000000000004";
    let forked_user = "019f6000-0000-7000-8000-000000000005";
    let rollout = codex_dir.join("sessions/rollout.jsonl");
    seed_thread_database(
        &database,
        &[
            (root_a, &rollout),
            (root_b, &rollout),
            (child, &rollout),
            (forked_user, &rollout),
        ],
        Some((root_a, child)),
    );
    let conn = Connection::open(&database).expect("open thread database");
    conn.execute_batch(
        "ALTER TABLE threads ADD COLUMN title TEXT;
             ALTER TABLE threads ADD COLUMN source TEXT;
             ALTER TABLE threads ADD COLUMN thread_source TEXT;
             UPDATE threads SET title = 'same title';",
    )
    .expect("extend thread schema");
    conn.execute(
        "UPDATE threads SET thread_source = 'subagent' WHERE id = ?1",
        [child],
    )
    .expect("mark child subagent");
    conn.execute(
        "UPDATE threads SET thread_source = 'user' WHERE id = ?1",
        [forked_user],
    )
    .expect("mark forked user thread");
    conn.execute(
        "INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id) VALUES (?1, ?2)",
        (root_a, forked_user),
    )
    .expect("insert user fork edge");
    conn.execute(
        "INSERT INTO threads (id, model_provider, rollout_path, title, source)
             VALUES (?1, 'openai', ?2, 'same title', ?3)",
        params![
            orphan_subagent,
            rollout.display().to_string(),
            r#"{"subagent":{"thread_spawn":{"depth":1}}}"#
        ],
    )
    .expect("insert source-marked subagent");
    drop(conn);

    let rollouts = scan_rollouts(&codex_dir, "openai").expect("scan rollouts");
    let scan = scan_sqlite(&codex_dir, &rollouts, "openai").expect("scan sqlite");
    assert_eq!(scan.sqlite_threads, 5);
    assert_eq!(scan.top_level_threads, 3);
    assert_eq!(scan.subagent_threads, 2);

    let (previews, warnings) =
        list_session_previews(&codex_dir, &rollouts, "openai", 50).expect("list previews");
    assert!(warnings.is_empty());
    assert_eq!(previews.iter().filter(|item| item.is_subagent).count(), 2);
    assert_eq!(
        previews
            .into_iter()
            .map(|item| item.id)
            .collect::<HashSet<_>>(),
        HashSet::from([
            root_a.to_string(),
            root_b.to_string(),
            child.to_string(),
            orphan_subagent.to_string(),
            forked_user.to_string(),
        ])
    );

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn session_previews_sort_globally_before_deduplicating_database_rows() {
    let codex_dir = temp_codex_dir("session-preview-database-dedup");
    let duplicate_id = "019f6000-0000-7000-8000-000000000201";
    let legacy_only_id = "019f6000-0000-7000-8000-000000000202";
    let rollout = codex_dir.join("sessions/rollout.jsonl");
    let current = codex_dir.join("sqlite/state_5.sqlite");
    let legacy = codex_dir.join("state_5.sqlite");
    seed_thread_database(&current, &[(duplicate_id, &rollout)], None);
    seed_thread_database(
        &legacy,
        &[(duplicate_id, &rollout), (legacy_only_id, &rollout)],
        None,
    );
    for database in [&current, &legacy] {
        Connection::open(database)
            .expect("open thread database")
            .execute_batch(
                "ALTER TABLE threads ADD COLUMN title TEXT;
                     ALTER TABLE threads ADD COLUMN updated_at_ms INTEGER;",
            )
            .expect("add preview columns");
    }
    Connection::open(&current)
        .expect("open current database")
        .execute(
            "UPDATE threads SET title = 'new copy', updated_at_ms = 300 WHERE id = ?1",
            [duplicate_id],
        )
        .expect("update current copy");
    let legacy_conn = Connection::open(&legacy).expect("open legacy database");
    legacy_conn
        .execute(
            "UPDATE threads SET title = 'old copy', updated_at_ms = 100 WHERE id = ?1",
            [duplicate_id],
        )
        .expect("update old copy");
    legacy_conn
        .execute(
            "UPDATE threads SET title = 'legacy only', updated_at_ms = 200 WHERE id = ?1",
            [legacy_only_id],
        )
        .expect("update legacy-only row");
    drop(legacy_conn);

    let rollouts = scan_rollouts(&codex_dir, "openai").expect("scan rollouts");
    let sqlite = scan_sqlite(&codex_dir, &rollouts, "openai").expect("scan sqlite");
    assert_eq!(sqlite.sqlite_threads, 2);
    assert_eq!(sqlite.top_level_threads, 2);
    let (previews, warnings) =
        list_session_previews(&codex_dir, &rollouts, "openai", 50).expect("list previews");
    assert!(warnings.is_empty());
    assert_eq!(previews.len(), 2);
    assert_eq!(previews[0].id, duplicate_id);
    assert_eq!(previews[0].title, "new copy");
    assert_eq!(previews[1].id, legacy_only_id);

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn local_session_delete_removes_duplicates_descendants_files_and_related_rows() {
    let codex_dir = temp_codex_dir("hard-delete-sessions");
    let parent_id = "019f6000-0000-7000-8000-000000000001";
    let child_id = "019f6000-0000-7000-8000-000000000002";
    let keep_id = "019f6000-0000-7000-8000-000000000003";
    let active_dir = codex_dir.join("sessions/2026/07/13");
    let archived_dir = codex_dir.join("archived_sessions/2026/07/13");
    fs::create_dir_all(&active_dir).expect("create active sessions");
    fs::create_dir_all(&archived_dir).expect("create archived sessions");
    let parent_rollout = active_dir.join(format!("rollout-test-{parent_id}.jsonl"));
    let child_rollout = archived_dir.join(format!("rollout-test-{child_id}.jsonl"));
    let child_compressed = archived_dir.join(format!("rollout-test-{child_id}.jsonl.zst"));
    let keep_rollout = active_dir.join(format!("rollout-test-{keep_id}.jsonl"));
    for (id, path) in [
        (parent_id, &parent_rollout),
        (child_id, &child_rollout),
        (keep_id, &keep_rollout),
    ] {
        write_text(
            path,
            &format!(r#"{{"type":"session_meta","payload":{{"id":"{id}"}}}}"#),
        )
        .expect("write rollout");
    }
    fs::write(&child_compressed, b"compressed-placeholder").expect("write zstd rollout");

    let current = codex_dir.join("state_5.sqlite");
    let legacy = codex_dir.join("sqlite/state_5.sqlite");
    seed_thread_database(
        &current,
        &[
            (parent_id, &parent_rollout),
            (child_id, &child_rollout),
            (keep_id, &keep_rollout),
        ],
        Some((parent_id, child_id)),
    );
    seed_thread_database(
        &legacy,
        &[(parent_id, &parent_rollout), (keep_id, &keep_rollout)],
        Some((parent_id, keep_id)),
    );

    let unrelated = codex_dir.join("unrelated.sqlite");
    let unrelated_conn = Connection::open(&unrelated).expect("open unrelated database");
    unrelated_conn
        .execute("CREATE TABLE logs (thread_id TEXT)", [])
        .expect("create unrelated table");
    unrelated_conn
        .execute("INSERT INTO logs (thread_id) VALUES (?1)", [parent_id])
        .expect("insert unrelated row");
    drop(unrelated_conn);

    let catalog = codex_dir.join("sqlite/codex-dev.db");
    let catalog_conn = Connection::open(&catalog).expect("open catalog");
    catalog_conn
        .execute_batch(
            "CREATE TABLE local_thread_catalog (thread_id TEXT);
                 CREATE TABLE automation_runs (thread_id TEXT);
                 CREATE TABLE inbox_items (thread_id TEXT);",
        )
        .expect("create catalog schema");
    for id in [parent_id, child_id, keep_id] {
        for table in ["local_thread_catalog", "automation_runs", "inbox_items"] {
            catalog_conn
                .execute(
                    &format!("INSERT INTO {table} (thread_id) VALUES (?1)"),
                    [id],
                )
                .expect("insert catalog reference");
        }
    }
    drop(catalog_conn);

    for (filename, table) in [
        ("logs_2.sqlite", "logs"),
        ("memories_1.sqlite", "stage1_outputs"),
        ("goals_1.sqlite", "thread_goals"),
    ] {
        let path = codex_dir.join(filename);
        let conn = Connection::open(path).expect("open related database");
        conn.execute(&format!("CREATE TABLE {table} (thread_id TEXT)"), [])
            .expect("create related schema");
        for id in [parent_id, child_id, keep_id] {
            conn.execute(
                &format!("INSERT INTO {table} (thread_id) VALUES (?1)"),
                [id],
            )
            .expect("insert related row");
        }
    }

    write_text(
            &codex_dir.join("session_index.jsonl"),
            &format!(
                "{{\"id\":\"{parent_id}\",\"thread_name\":\"parent\"}}\nnot-json\n{{\"id\":\"{child_id}\",\"thread_name\":\"child\"}}\n{{\"id\":\"{keep_id}\",\"thread_name\":\"keep\"}}\n"
            ),
        )
        .expect("write session index");
    write_text(
            &codex_dir.join("history.jsonl"),
            &format!(
                "{{\"session_id\":\"{parent_id}\",\"text\":\"parent secret\"}}\ninvalid-history\n{{\"session_id\":\"{child_id}\",\"text\":\"child secret\"}}\n{{\"session_id\":\"{keep_id}\",\"text\":\"keep\"}}\n"
            ),
        )
        .expect("write session history");
    let snapshots = codex_dir.join("shell_snapshots");
    fs::create_dir_all(&snapshots).expect("create shell snapshots");
    let parent_snapshot = snapshots.join(format!("{parent_id}.100.sh"));
    let child_snapshot = snapshots.join(format!("{child_id}.200.sh"));
    let keep_snapshot = snapshots.join(format!("{keep_id}.300.sh"));
    fs::write(&parent_snapshot, "parent").expect("write parent snapshot");
    fs::write(&child_snapshot, "child").expect("write child snapshot");
    fs::write(&keep_snapshot, "keep").expect("write keep snapshot");

    let result = hard_delete_sessions_locally(&codex_dir, &[parent_id.to_string()])
        .expect("hard delete parent session");

    assert!(result.errors.is_empty());
    assert_eq!(result.deleted_ids.len(), 2);
    assert!(result.deleted_ids.contains(parent_id));
    assert!(result.deleted_ids.contains(child_id));
    assert_eq!(result.deleted_thread_rows, 3);
    assert_eq!(result.deleted_rollout_files, 3);
    assert!(!parent_rollout.exists());
    assert!(!child_rollout.exists());
    assert!(!child_compressed.exists());
    assert!(keep_rollout.exists());
    assert_eq!(sqlite_count(&current, "SELECT COUNT(*) FROM threads"), 1);
    assert_eq!(sqlite_count(&legacy, "SELECT COUNT(*) FROM threads"), 1);
    assert_eq!(
        sqlite_count(
            &current,
            "SELECT COUNT(*) FROM agent_job_items WHERE assigned_thread_id IS NOT NULL"
        ),
        1
    );
    assert_eq!(
        sqlite_count(&catalog, "SELECT COUNT(*) FROM local_thread_catalog"),
        1
    );
    assert_eq!(
        sqlite_count(
            &codex_dir.join("logs_2.sqlite"),
            "SELECT COUNT(*) FROM logs"
        ),
        1
    );
    assert_eq!(
        sqlite_count(
            &codex_dir.join("memories_1.sqlite"),
            "SELECT COUNT(*) FROM stage1_outputs"
        ),
        1
    );
    assert_eq!(
        sqlite_count(
            &codex_dir.join("goals_1.sqlite"),
            "SELECT COUNT(*) FROM thread_goals"
        ),
        1
    );
    assert_eq!(sqlite_count(&unrelated, "SELECT COUNT(*) FROM logs"), 1);
    let index = fs::read_to_string(codex_dir.join("session_index.jsonl"))
        .expect("read filtered session index");
    assert!(!index.contains(parent_id));
    assert!(!index.contains(child_id));
    assert!(index.contains(keep_id));
    assert!(index.contains("not-json"));
    let history =
        fs::read_to_string(codex_dir.join("history.jsonl")).expect("read filtered history");
    assert!(!history.contains("parent secret"));
    assert!(!history.contains("child secret"));
    assert!(history.contains(keep_id));
    assert!(history.contains("invalid-history"));
    assert!(!parent_snapshot.exists());
    assert!(!child_snapshot.exists());
    assert!(keep_snapshot.exists());

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn local_session_delete_reports_partial_database_cleanup() {
    let codex_dir = temp_codex_dir("hard-delete-partial-database");
    let id = "019f6000-0000-7000-8000-000000000020";
    let session_dir = codex_dir.join("sessions/2026/07/13");
    fs::create_dir_all(&session_dir).expect("create sessions directory");
    let rollout = session_dir.join(format!("rollout-test-{id}.jsonl"));
    write_text(&rollout, "session").expect("write rollout");
    let current = codex_dir.join("state_5.sqlite");
    seed_thread_database(&current, &[(id, &rollout)], None);

    let blocked = codex_dir.join("logs_3.sqlite");
    let conn = Connection::open(&blocked).expect("open blocked related database");
    conn.execute_batch(
        "CREATE TABLE logs (thread_id TEXT);
             INSERT INTO logs (thread_id) VALUES ('019f6000-0000-7000-8000-000000000020');
             CREATE TRIGGER block_log_delete BEFORE DELETE ON logs
             BEGIN SELECT RAISE(ABORT, 'blocked cleanup'); END;",
    )
    .expect("create blocked cleanup schema");
    drop(conn);

    let result = hard_delete_sessions_locally(&codex_dir, &[id.to_string()])
        .expect("return partial cleanup result");

    assert!(!rollout.exists());
    assert_eq!(sqlite_count(&current, "SELECT COUNT(*) FROM threads"), 0);
    assert_eq!(sqlite_count(&blocked, "SELECT COUNT(*) FROM logs"), 1);
    assert_eq!(result.errors.len(), 1);
    assert!(result.errors[0].contains("blocked cleanup"));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn local_session_delete_rejects_rollout_outside_codex_session_roots() {
    let codex_dir = temp_codex_dir("hard-delete-path-guard");
    let id = "019f6000-0000-7000-8000-000000000010";
    let outside_dir = temp_codex_dir("hard-delete-outside");
    let outside = outside_dir.join(format!("rollout-test-{id}.jsonl"));
    write_text(&outside, "outside").expect("write outside rollout");
    let current = codex_dir.join("state_5.sqlite");
    seed_thread_database(&current, &[(id, &outside)], None);

    let error = hard_delete_sessions_locally(&codex_dir, &[id.to_string()])
        .expect_err("reject external rollout path");
    assert!(error.to_string().contains("超出 Codex 会话目录"));
    assert!(outside.exists());
    assert_eq!(sqlite_count(&current, "SELECT COUNT(*) FROM threads"), 1);

    let _ = fs::remove_dir_all(codex_dir);
    let _ = fs::remove_dir_all(outside_dir);
}

#[cfg(unix)]
#[test]
fn local_session_delete_does_not_follow_rollout_directory_symlinks() {
    use std::os::unix::fs::symlink;

    let codex_dir = temp_codex_dir("hard-delete-symlink-guard");
    let id = "019f6000-0000-7000-8000-000000000011";
    let outside_dir = temp_codex_dir("hard-delete-symlink-outside");
    let outside = outside_dir.join(format!("rollout-test-{id}.jsonl"));
    write_text(&outside, "outside").expect("write outside rollout");

    let sessions_dir = codex_dir.join("sessions");
    fs::create_dir_all(&sessions_dir).expect("create sessions directory");
    symlink(&outside_dir, sessions_dir.join("external")).expect("create directory symlink");

    let missing_rollout = sessions_dir.join(format!("missing/rollout-test-{id}.jsonl"));
    let current = codex_dir.join("state_5.sqlite");
    seed_thread_database(&current, &[(id, &missing_rollout)], None);

    let result = hard_delete_sessions_locally(&codex_dir, &[id.to_string()])
        .expect("delete database row without following symlink");
    assert_eq!(result.deleted_thread_rows, 1);
    assert_eq!(result.deleted_rollout_files, 0);
    assert!(outside.exists());

    let _ = fs::remove_dir_all(codex_dir);
    let _ = fs::remove_dir_all(outside_dir);
}
