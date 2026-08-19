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
                source TEXT NOT NULL DEFAULT 'manual',
                source_id TEXT,
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
fn provider_identity_uses_the_complete_profile_but_not_authenticated_name() {
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
        "gpt-5.6-sol",
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
        "gpt-5.6-sol",
        None,
    );
    let different_model = provider_fixture(
        "different-model",
        "Magic AI",
        "https://example.com/v1",
        Some("sk-same"),
        "deepseek-v3",
        None,
    );
    assert_eq!(provider_identity(&direct), provider_identity(&toml));
    assert_ne!(
        provider_identity(&direct),
        provider_identity(&different_key)
    );
    assert_ne!(
        provider_identity(&direct),
        provider_identity(&different_model)
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
        "one",
        None,
    );
    let anonymous_other_model = provider_fixture(
        "anonymous-other-model",
        "acme api",
        "https://example.com/v1",
        None,
        "two",
        None,
    );
    assert_eq!(
        provider_identity(&anonymous_a),
        provider_identity(&anonymous_b)
    );
    assert_ne!(
        provider_identity(&anonymous_a),
        provider_identity(&anonymous_other_model)
    );
}

#[test]
fn manual_provider_save_keeps_distinct_profiles_for_models_and_keys() {
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
    let second_add = upsert_provider_on_connection(&conn, renamed, ProviderUpsertMode::Manual)
        .expect("keep a different model profile");
    assert_eq!(second_add.kind, ProviderUpsertKind::Added);
    assert_eq!(second_add.provider.id, "second");
    assert_eq!(second_add.provider.provider_name, "Second Name");
    assert_eq!(second_add.provider.model, "model-b");

    let other_key = normalize_saved_provider(provider_fixture(
        "third",
        "Second Name",
        "https://example.com/v1",
        Some("sk-other"),
        "model-b",
        None,
    ))
    .expect("normalize other key");
    let third_add = upsert_provider_on_connection(&conn, other_key, ProviderUpsertMode::Manual)
        .expect("keep different credential");
    assert_eq!(third_add.kind, ProviderUpsertKind::Added);
    assert_eq!(list_saved_providers_on_connection(&conn).unwrap().len(), 3);
}

#[test]
fn imported_provider_merge_keeps_local_id_and_applies_complete_source() {
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

    let imported_toml = r#"# authoritative cc-switch template
model_provider = "custom"
model = "local-model"
service_tier = "priority"

[model_providers.custom]
name = "CC Name"
base_url = "https://example.com/v1"
wire_api = "responses"
requires_openai_auth = true

[projects."/work/project"]
trust_level = "trusted"
"#;
    let imported_fixture = provider_fixture(
        "cc-switch-id",
        "CC Name",
        "https://EXAMPLE.com:443/v1/",
        Some("sk-same"),
        "local-model",
        Some(imported_toml),
    );
    let imported = normalize_saved_provider(imported_fixture).expect("normalize import");
    let result = upsert_ccswitch_provider_on_connection(&conn, imported.clone(), "cc-switch-id")
        .expect("merge import");
    assert_eq!(result.kind, ProviderUpsertKind::Merged);
    assert_eq!(result.provider.id, "local");
    assert_eq!(result.provider.provider_name, "CC Name");
    assert_eq!(result.provider.model, "local-model");
    assert!(result.provider.requires_openai_auth);
    let normalized_toml = result.provider.toml_config.as_deref().unwrap();
    assert!(normalized_toml.contains("# authoritative cc-switch template"));
    assert!(normalized_toml.contains("name = \"CC Name\""));
    assert!(normalized_toml.contains("service_tier = \"priority\""));
    assert!(normalized_toml.contains("[projects.\"/work/project\"]"));
    assert!(normalized_toml.contains("wire_api = \"responses\""));
    assert!(!normalized_toml.contains("experimental_bearer_token"));
    assert_eq!(list_saved_providers_on_connection(&conn).unwrap().len(), 1);

    let repeated = upsert_ccswitch_provider_on_connection(&conn, imported, "cc-switch-id")
        .expect("repeat identical import");
    assert_eq!(repeated.provider.id, "local");
    assert_eq!(
        repeated.provider.toml_config.as_deref(),
        Some(normalized_toml)
    );
    assert_eq!(list_saved_providers_on_connection(&conn).unwrap().len(), 1);
}

#[test]
fn provider_migration_keeps_the_latest_exact_credential_record() {
    let conn = provider_test_connection();
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
        "local-model",
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

    assert_eq!(
        list_saved_providers_on_connection(&conn).unwrap().len(),
        5,
        "read-only listing must not delete legacy duplicates"
    );
    assert_eq!(
        consolidate_legacy_provider_duplicates_on_connection(&conn).unwrap(),
        1
    );
    let rows = list_saved_providers_on_connection(&conn).unwrap();
    assert_eq!(rows.len(), 4);
    let survivor = rows.iter().find(|row| row.id == "later-id").unwrap();
    assert_eq!(survivor.provider_name, "Imported Name");
    assert_eq!(survivor.model, "local-model");
    assert_eq!(
        survivor.toml_config.as_deref(),
        Some("local preserved toml")
    );
    assert!(rows.iter().any(|row| row.id == "different-key"));
    assert!(rows.iter().any(|row| row.id == "anonymous-a"));
    assert!(rows.iter().any(|row| row.id == "anonymous-b"));
    assert!(!rows.iter().any(|row| row.id == "first-id"));
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
fn ccswitch_official_auth_reader_supports_legacy_schema_without_category() {
    let db_path = temp_codex_dir("legacy-ccswitch-official").join("cc-switch.db");
    let conn = Connection::open(&db_path).expect("open legacy cc-switch database");
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
            INSERT INTO providers
                (id, app_type, name, settings_config, sort_index, created_at)
            VALUES
                ('codex-official', 'codex', 'OpenAI Official',
                 '{\"auth\":{\"auth_mode\":\"chatgpt\",\"tokens\":{\"access_token\":\"token\"}}}',
                 0, 1);",
    )
    .expect("seed legacy official provider");
    drop(conn);

    assert!(
        read_ccswitch_official_auth_inner(Some(db_path.display().to_string()))
            .expect("read legacy official auth")
            .is_some()
    );
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
        note: None,
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
        note: None,
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
fn prompt_enable_tolerates_malformed_unrelated_auth() {
    let codex_dir = temp_codex_dir("prompt-final-state-rollback");
    write_text(&config_path(&codex_dir), "model = \"before\"\n").expect("write config");
    write_text(&auth_path(&codex_dir), "{malformed-auth").expect("write malformed auth");
    let agents_before = install_managed_agents_block_in_content(
        "# User AGENTS\nkeep this\n",
        "builtin:previous",
        "previous managed prompt",
    )
    .expect("build managed AGENTS fixture");
    write_text(&agents_path(&codex_dir), &agents_before).expect("write AGENTS fixture");
    let target = codex_dir.join(INSTRUCTION_FILENAME);

    let result = enable_prompt_content_inner(
        Some(codex_dir.display().to_string()),
        INSTRUCTION_FILENAME,
        "replacement prompt",
        "builtin:gpt5.5-unrestricted",
        "managed",
        "test",
        PromptInjectionMode::Replace,
        "test-malformed-auth-tolerance",
    )
    .expect("malformed unrelated auth must not block prompt changes");

    assert!(result.state.instruction_enabled);
    assert!(fs::read_to_string(config_path(&codex_dir))
        .expect("read updated config")
        .contains("model_instructions_file = \"./gpt5.5-unrestricted.md\""));
    assert_eq!(
        fs::read_to_string(agents_path(&codex_dir)).expect("read preserved AGENTS"),
        "# User AGENTS\nkeep this\n"
    );
    assert_eq!(
        fs::read_to_string(target).expect("read enabled prompt"),
        "replacement prompt"
    );
    assert_eq!(
        fs::read_to_string(auth_path(&codex_dir)).expect("read preserved auth"),
        "{malformed-auth"
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
fn restore_backup_rolls_back_all_live_files_when_restored_auth_is_malformed() {
    let codex_dir = temp_codex_dir("restore-malformed-auth-rollback");
    write_text(&config_path(&codex_dir), "model = \"backup-model\"\n")
        .expect("write backup config");
    write_text(
        &auth_path(&codex_dir),
        "{\"auth_mode\":\"chatgpt\",\"tokens\":{\"access_token\":\"backup\"}}\n",
    )
    .expect("write backup auth");
    write_text(&agents_path(&codex_dir), "# Backup AGENTS\n").expect("write backup agents");
    let backup_id = create_backup(&codex_dir, "malformed-auth-target")
        .expect("create target backup")
        .expect("target backup id");
    let backup_dir = action_backup_root(&codex_dir)
        .expect("resolve backup root")
        .join(&backup_id);
    fs::write(backup_dir.join("auth.json"), b"{malformed-auth").expect("corrupt backup auth");

    write_text(&config_path(&codex_dir), "model = \"current-model\"\n")
        .expect("write current config");
    write_text(
        &auth_path(&codex_dir),
        "{\"auth_mode\":\"chatgpt\",\"tokens\":{\"access_token\":\"current\"}}\n",
    )
    .expect("write current auth");
    write_text(&agents_path(&codex_dir), "# Current AGENTS\n").expect("write current agents");
    let config_before = fs::read(config_path(&codex_dir)).expect("snapshot current config");
    let auth_before = fs::read(auth_path(&codex_dir)).expect("snapshot current auth");
    let agents_before = fs::read(agents_path(&codex_dir)).expect("snapshot current agents");

    restore_backup_inner(Some(codex_dir.display().to_string()), backup_id)
        .expect_err("malformed restored auth must fail");

    assert_eq!(
        fs::read(config_path(&codex_dir)).expect("read rolled back config"),
        config_before
    );
    assert_eq!(
        fs::read(auth_path(&codex_dir)).expect("read rolled back auth"),
        auth_before
    );
    assert_eq!(
        fs::read(agents_path(&codex_dir)).expect("read rolled back agents"),
        agents_before
    );
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn restore_backup_rejects_malformed_metadata_without_touching_live_files() {
    let codex_dir = temp_codex_dir("restore-malformed-meta");
    write_text(&config_path(&codex_dir), "model = \"current\"\n").expect("write config");
    write_text(&auth_path(&codex_dir), "{\"OPENAI_API_KEY\":\"current\"}\n").expect("write auth");
    write_text(&agents_path(&codex_dir), "# Current AGENTS\n").expect("write agents");
    let backup_id = create_backup(&codex_dir, "malformed-meta")
        .expect("create backup")
        .expect("backup id");
    let backup_dir = action_backup_root(&codex_dir)
        .expect("resolve backup root")
        .join(&backup_id);
    fs::write(backup_dir.join("meta.json"), b"{malformed-meta").expect("corrupt backup metadata");
    let config_before = fs::read(config_path(&codex_dir)).expect("snapshot config");
    let auth_before = fs::read(auth_path(&codex_dir)).expect("snapshot auth");
    let agents_before = fs::read(agents_path(&codex_dir)).expect("snapshot agents");

    let error = restore_backup_inner(Some(codex_dir.display().to_string()), backup_id)
        .expect_err("malformed metadata must fail");

    assert!(error.to_string().contains("meta.json"));
    assert_eq!(fs::read(config_path(&codex_dir)).unwrap(), config_before);
    assert_eq!(fs::read(auth_path(&codex_dir)).unwrap(), auth_before);
    assert_eq!(fs::read(agents_path(&codex_dir)).unwrap(), agents_before);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn restore_backup_rejects_path_traversal_ids() {
    let codex_dir = temp_codex_dir("restore-path-traversal");
    let error = restore_backup_inner(
        Some(codex_dir.display().to_string()),
        "../outside".to_string(),
    )
    .expect_err("path traversal backup id must fail");
    assert!(error.to_string().contains("备份 ID 无效"));
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn restore_backup_rejects_a_backup_from_another_codex_home() {
    let source = temp_codex_dir("restore-cross-home-source");
    let target = temp_codex_dir("restore-cross-home-target");
    write_text(&config_path(&source), "model = \"source\"\n").expect("write source config");
    write_text(&auth_path(&source), "{\"OPENAI_API_KEY\":\"source\"}\n")
        .expect("write source auth");
    write_text(&agents_path(&source), "# Source AGENTS\n").expect("write source agents");
    let backup_id = create_backup(&source, "cross-home")
        .expect("create source backup")
        .expect("source backup id");
    let source_backup = action_backup_root(&source)
        .expect("resolve source backup root")
        .join(&backup_id);
    let target_backup = action_backup_root(&target)
        .expect("resolve target backup root")
        .join(&backup_id);
    fs::create_dir_all(&target_backup).expect("create copied backup directory");
    for filename in ["meta.json", "config.toml", "auth.json", AGENTS_FILENAME] {
        fs::copy(source_backup.join(filename), target_backup.join(filename))
            .expect("copy source backup file");
    }

    write_text(&config_path(&target), "model = \"target\"\n").expect("write target config");
    write_text(&auth_path(&target), "{\"OPENAI_API_KEY\":\"target\"}\n")
        .expect("write target auth");
    write_text(&agents_path(&target), "# Target AGENTS\n").expect("write target agents");
    let config_before = fs::read(config_path(&target)).expect("snapshot target config");
    let auth_before = fs::read(auth_path(&target)).expect("snapshot target auth");
    let agents_before = fs::read(agents_path(&target)).expect("snapshot target agents");

    let error = restore_backup_inner(Some(target.display().to_string()), backup_id)
        .expect_err("cross-CODEX_HOME restore must fail");

    assert!(error.to_string().contains("其他 CODEX_HOME"));
    assert_eq!(fs::read(config_path(&target)).unwrap(), config_before);
    assert_eq!(fs::read(auth_path(&target)).unwrap(), auth_before);
    assert_eq!(fs::read(agents_path(&target)).unwrap(), agents_before);
    let _ = fs::remove_dir_all(source);
    let _ = fs::remove_dir_all(target);
}

#[test]
fn restore_backup_rejects_missing_metadata_without_touching_live_files() {
    let codex_dir = temp_codex_dir("restore-missing-meta");
    write_text(&config_path(&codex_dir), "model = \"backup\"\n").expect("write backup config");
    let backup_id = create_backup(&codex_dir, "missing-meta")
        .expect("create backup")
        .expect("backup id");
    let backup_dir = action_backup_root(&codex_dir)
        .expect("resolve backup root")
        .join(&backup_id);
    fs::remove_file(backup_dir.join("meta.json")).expect("remove backup metadata");
    write_text(&config_path(&codex_dir), "model = \"current\"\n").expect("write current config");
    let config_before = fs::read(config_path(&codex_dir)).expect("snapshot config");

    let error = restore_backup_inner(Some(codex_dir.display().to_string()), backup_id)
        .expect_err("missing metadata must fail");

    assert!(error.to_string().contains("缺少元数据"));
    assert_eq!(fs::read(config_path(&codex_dir)).unwrap(), config_before);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn restore_backup_rejects_each_declared_but_missing_file_before_live_changes() {
    let codex_dir = temp_codex_dir("restore-declared-file-missing");
    write_text(&config_path(&codex_dir), "model = \"backup\"\n").expect("write backup config");
    write_text(&auth_path(&codex_dir), "{\"OPENAI_API_KEY\":\"backup\"}\n")
        .expect("write backup auth");
    write_text(&agents_path(&codex_dir), "# Backup AGENTS\n").expect("write backup agents");
    let backup_id = create_backup(&codex_dir, "declared-files")
        .expect("create backup")
        .expect("backup id");
    let backup_dir = action_backup_root(&codex_dir)
        .expect("resolve backup root")
        .join(&backup_id);
    write_text(&config_path(&codex_dir), "model = \"current\"\n").expect("write current config");
    write_text(&auth_path(&codex_dir), "{\"OPENAI_API_KEY\":\"current\"}\n")
        .expect("write current auth");
    write_text(&agents_path(&codex_dir), "# Current AGENTS\n").expect("write current agents");
    let config_before = fs::read(config_path(&codex_dir)).expect("snapshot config");
    let auth_before = fs::read(auth_path(&codex_dir)).expect("snapshot auth");
    let agents_before = fs::read(agents_path(&codex_dir)).expect("snapshot agents");

    for filename in ["config.toml", "auth.json", AGENTS_FILENAME] {
        let path = backup_dir.join(filename);
        let bytes = fs::read(&path).expect("snapshot declared backup file");
        fs::remove_file(&path).expect("remove declared backup file");

        let error = restore_backup_inner(Some(codex_dir.display().to_string()), backup_id.clone())
            .expect_err("missing declared backup file must fail");

        assert!(error.to_string().contains(filename));
        assert_eq!(fs::read(config_path(&codex_dir)).unwrap(), config_before);
        assert_eq!(fs::read(auth_path(&codex_dir)).unwrap(), auth_before);
        assert_eq!(fs::read(agents_path(&codex_dir)).unwrap(), agents_before);
        fs::write(path, bytes).expect("restore declared backup fixture");
    }
    let _ = fs::remove_dir_all(codex_dir);
}

#[cfg(unix)]
#[test]
fn restore_backup_rejects_a_declared_symlink_file_before_live_changes() {
    use std::os::unix::fs::symlink;

    let codex_dir = temp_codex_dir("restore-declared-symlink");
    write_text(&config_path(&codex_dir), "model = \"backup\"\n").expect("write backup config");
    let backup_id = create_backup(&codex_dir, "declared-symlink")
        .expect("create backup")
        .expect("backup id");
    let backup_dir = action_backup_root(&codex_dir)
        .expect("resolve backup root")
        .join(&backup_id);
    let backup_config = backup_dir.join("config.toml");
    fs::remove_file(&backup_config).expect("remove ordinary backup config");
    let linked_target = backup_dir.join("linked-config.toml");
    fs::write(&linked_target, "model = \"linked\"\n").expect("write symlink target");
    symlink(&linked_target, &backup_config).expect("create backup config symlink");
    write_text(&config_path(&codex_dir), "model = \"current\"\n").expect("write current config");
    let config_before = fs::read(config_path(&codex_dir)).expect("snapshot current config");

    let error = restore_backup_inner(Some(codex_dir.display().to_string()), backup_id)
        .expect_err("declared symlink backup file must fail");

    assert!(error.to_string().contains("不是普通文件"));
    assert_eq!(fs::read(config_path(&codex_dir)).unwrap(), config_before);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn restore_backup_rejects_extra_payload_files_when_presence_flags_are_false() {
    let codex_dir = temp_codex_dir("restore-false-presence-flags");
    let backup_id = create_backup(&codex_dir, "absent-files")
        .expect("create empty backup")
        .expect("backup id");
    let backup_dir = action_backup_root(&codex_dir)
        .expect("resolve backup root")
        .join(&backup_id);
    write_text(&config_path(&codex_dir), "model = \"current\"\n").expect("write current config");
    write_text(&auth_path(&codex_dir), "{\"OPENAI_API_KEY\":\"current\"}\n")
        .expect("write current auth");
    write_text(&agents_path(&codex_dir), "# Current AGENTS\n").expect("write current agents");
    let config_before = fs::read(config_path(&codex_dir)).expect("snapshot config");
    let auth_before = fs::read(auth_path(&codex_dir)).expect("snapshot auth");
    let agents_before = fs::read(agents_path(&codex_dir)).expect("snapshot agents");

    for (filename, contents) in [
        ("config.toml", "model = \"extra\"\n"),
        ("auth.json", "{\"OPENAI_API_KEY\":\"extra\"}\n"),
        (AGENTS_FILENAME, "# Extra AGENTS\n"),
    ] {
        let extra = backup_dir.join(filename);
        fs::write(&extra, contents).expect("write extra backup payload");

        let error = restore_backup_inner(Some(codex_dir.display().to_string()), backup_id.clone())
            .expect_err("undeclared backup payload must fail");

        assert!(error.to_string().contains("多余文件"));
        assert!(error.to_string().contains(filename));
        assert_eq!(fs::read(config_path(&codex_dir)).unwrap(), config_before);
        assert_eq!(fs::read(auth_path(&codex_dir)).unwrap(), auth_before);
        assert_eq!(fs::read(agents_path(&codex_dir)).unwrap(), agents_before);
        fs::remove_file(extra).expect("remove extra backup payload");
    }

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn restore_backup_deletes_live_files_declared_absent_from_a_clean_backup() {
    let codex_dir = temp_codex_dir("restore-clean-absent-files");
    let backup_id = create_backup(&codex_dir, "absent-files")
        .expect("create empty backup")
        .expect("backup id");
    write_text(&config_path(&codex_dir), "model = \"current\"\n").expect("write current config");
    write_text(&auth_path(&codex_dir), "{\"OPENAI_API_KEY\":\"current\"}\n")
        .expect("write current auth");
    write_text(&agents_path(&codex_dir), "# Current AGENTS\n").expect("write current agents");

    restore_backup_inner(Some(codex_dir.display().to_string()), backup_id)
        .expect("restore clean declared-absent files");

    assert!(!config_path(&codex_dir).exists());
    assert!(!auth_path(&codex_dir).exists());
    assert!(!agents_path(&codex_dir).exists());
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn restore_backup_rejects_metadata_id_mismatch_without_touching_live_files() {
    let codex_dir = temp_codex_dir("restore-mismatched-meta-id");
    write_text(&config_path(&codex_dir), "model = \"backup\"\n").expect("write backup config");
    write_text(&auth_path(&codex_dir), "{\"OPENAI_API_KEY\":\"backup\"}\n")
        .expect("write backup auth");
    write_text(&agents_path(&codex_dir), "# Backup AGENTS\n").expect("write backup agents");
    let backup_id = create_backup(&codex_dir, "mismatched-meta-id")
        .expect("create backup")
        .expect("backup id");
    let meta_path = action_backup_root(&codex_dir)
        .expect("resolve backup root")
        .join(&backup_id)
        .join("meta.json");
    let mut meta: Value = serde_json::from_slice(&fs::read(&meta_path).expect("read metadata"))
        .expect("parse metadata");
    meta["id"] = json!("different-backup-id");
    write_json(&meta_path, &meta).expect("write mismatched metadata id");

    write_text(&config_path(&codex_dir), "model = \"current\"\n").expect("write current config");
    write_text(&auth_path(&codex_dir), "{\"OPENAI_API_KEY\":\"current\"}\n")
        .expect("write current auth");
    write_text(&agents_path(&codex_dir), "# Current AGENTS\n").expect("write current agents");
    let config_before = fs::read(config_path(&codex_dir)).expect("snapshot config");
    let auth_before = fs::read(auth_path(&codex_dir)).expect("snapshot auth");
    let agents_before = fs::read(agents_path(&codex_dir)).expect("snapshot agents");

    let error = restore_backup_inner(Some(codex_dir.display().to_string()), backup_id)
        .expect_err("mismatched metadata id must fail");

    assert!(error.to_string().contains("ID 与请求不一致"));
    assert_eq!(fs::read(config_path(&codex_dir)).unwrap(), config_before);
    assert_eq!(fs::read(auth_path(&codex_dir)).unwrap(), auth_before);
    assert_eq!(fs::read(agents_path(&codex_dir)).unwrap(), agents_before);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn backup_codex_home_identity_accepts_equivalent_missing_paths() {
    let root = temp_codex_dir("backup-missing-path-identity");
    let missing = root.join("future").join("codex-home");
    let equivalent = root
        .join("future")
        .join("nested")
        .join("..")
        .join("codex-home");
    let meta = BackupMeta {
        id: "missing-path".to_string(),
        action: "test".to_string(),
        created_at: Local::now().to_rfc3339(),
        codex_dir: equivalent.display().to_string(),
        config_path: String::new(),
        auth_path: String::new(),
        had_config: false,
        had_auth: false,
        agents_path: String::new(),
        had_agents: false,
        tracks_agents: true,
    };

    validate_backup_codex_dir(&meta, &missing)
        .expect("equivalent missing CODEX_HOME paths must match");
    assert!(!missing.exists());
    let _ = fs::remove_dir_all(root);
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
fn switch_provider_round_trip_replaces_live_auth_and_restores_official_snapshot() {
    let codex_dir = temp_codex_dir("switch-provider");
    let official_auth = json!({
        "OPENAI_API_KEY": null,
        "tokens": {
            "access_token": "official-access-token",
            "refresh_token": "official-refresh-token"
        },
        "last_refresh": "2026-07-24T00:00:00Z"
    });
    write_json(&auth_path(&codex_dir), &official_auth).expect("write official auth");
    let switch_to_magic = || {
        switch_provider_inner(ProviderInput {
            config_dir: Some(codex_dir.display().to_string()),
            provider_id: Some("magicai".to_string()),
            provider_name: "MagicAI".to_string(),
            base_url: "https://example.com/v1/".to_string(),
            model: "gpt-5.5".to_string(),
            api_key: Some("sk-test".to_string()),
            wire_api: Some("responses".to_string()),
            requires_openai_auth: None,
        })
    };
    let result = switch_to_magic().expect("switch provider");

    assert_eq!(result.message, "已切换到 MagicAI");
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
    assert!(config_doc.get("experimental_bearer_token").is_none());
    assert!(config_doc["model_providers"]["custom"]
        .get("experimental_bearer_token")
        .is_none());
    let provider_auth: Value = serde_json::from_slice(
        &fs::read(auth_path(&codex_dir)).expect("read auth after provider switch"),
    )
    .expect("parse provider auth");
    assert_eq!(provider_auth, json!({"OPENAI_API_KEY": "sk-test"}));

    let official = switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("switch back to official");
    assert_eq!(official.message, "已切换到 OpenAI Official");
    assert_eq!(official.state.model_provider.as_deref(), Some("custom"));
    assert!(official.state.is_official_provider);
    let restored_auth: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read restored official auth"),
    )
    .expect("parse restored official auth");
    assert_eq!(restored_auth, official_auth);

    let switched_again = switch_to_magic().expect("switch back to provider after official");
    assert_eq!(switched_again.message, "已切换到 MagicAI");
    assert!(!switched_again.state.is_official_provider);
    assert_eq!(
        switched_again.state.model_provider.as_deref(),
        Some("custom")
    );
    let final_config = fs::read_to_string(config_path(&codex_dir)).expect("read final config");
    let final_doc = final_config
        .parse::<DocumentMut>()
        .expect("parse final provider config");
    assert_eq!(
        final_doc["model_providers"]["custom"]["requires_openai_auth"].as_bool(),
        Some(true)
    );
    assert!(final_doc["model_providers"]["custom"]
        .get("experimental_bearer_token")
        .is_none());
    let final_auth: Value =
        serde_json::from_slice(&fs::read(auth_path(&codex_dir)).expect("read final provider auth"))
            .expect("parse final provider auth");
    assert_eq!(final_auth, json!({"OPENAI_API_KEY": "sk-test"}));

    let snapshot_path = providers::official_snapshot_path_for_test(&codex_dir)
        .expect("resolve official snapshot path");
    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn third_party_auth_shapes_round_trip_to_pure_key_and_complete_toml() {
    let _db_guard = crate::app_db::test_db_guard();
    let cases = [
        ("key-only", json!({"OPENAI_API_KEY": "sk-provider"})),
        (
            "legacy-auth-mode",
            json!({"OPENAI_API_KEY": "sk-provider", "auth_mode": "apikey"}),
        ),
    ];

    for (label, initial_provider_auth) in cases {
        let codex_dir = temp_codex_dir(&format!("third-party-auth-round-trip-{label}"));
        let official_config = r#"model_provider = "openai"
model = "official-model"
"#;
        let official_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": "official-access-token",
                "refresh_token": "official-refresh-token"
            }
        });
        write_text(&config_path(&codex_dir), official_config).expect("write official config");
        write_json(&auth_path(&codex_dir), &official_auth).expect("write official auth");
        assert!(
            providers::capture_live_chatgpt_config(&codex_dir).expect("capture official snapshot")
        );

        let provider_config = r#"# preserve-provider-template
model_provider = "custom"
model = "gpt-5.6-sol"
model_reasoning_effort = "xhigh"
service_tier = "priority"
disable_response_storage = true
notify = ["C:\\Tools\\notify.exe", "turn-ended"]

[model_providers.custom]
name = "Round Trip Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
experimental_bearer_token = "sk-legacy-scoped"
request_max_retries = 11

[desktop]
followUpQueueMode = "queue"
localeOverride = "zh-CN"

[features]
js_repl = false
"#;
        write_text(&config_path(&codex_dir), provider_config).expect("write provider config");
        write_json(&auth_path(&codex_dir), &initial_provider_auth)
            .expect("write initial provider auth");

        let official = switch_official_provider_inner(Some(codex_dir.display().to_string()))
            .expect("switch provider to official");
        assert!(official.state.is_official_provider, "case {label}");
        assert_eq!(
            serde_json::from_slice::<Value>(
                &fs::read(auth_path(&codex_dir)).expect("read restored official auth")
            )
            .expect("parse restored official auth"),
            official_auth,
            "case {label}"
        );

        let provider = save_provider_toml_config_inner(ProviderTomlInput {
            config_dir: Some(codex_dir.display().to_string()),
            config_text: provider_config.to_string(),
            api_key: Some("sk-provider".to_string()),
        })
        .expect("switch official back to provider");
        assert!(!provider.state.is_official_provider, "case {label}");
        assert_eq!(provider.state.model.as_deref(), Some("gpt-5.6-sol"));

        let final_auth: Value = serde_json::from_slice(
            &fs::read(auth_path(&codex_dir)).expect("read final provider auth"),
        )
        .expect("parse final provider auth");
        assert_eq!(
            final_auth,
            json!({"OPENAI_API_KEY": "sk-provider"}),
            "case {label}"
        );

        let final_config =
            fs::read_to_string(config_path(&codex_dir)).expect("read final provider config");
        let final_doc = final_config
            .parse::<DocumentMut>()
            .expect("parse final provider config");
        assert!(final_config.contains("# preserve-provider-template"));
        assert_eq!(final_doc["model"].as_str(), Some("gpt-5.6-sol"));
        assert_eq!(final_doc["model_reasoning_effort"].as_str(), Some("xhigh"));
        assert_eq!(final_doc["service_tier"].as_str(), Some("priority"));
        assert_eq!(
            final_doc["model_providers"]["custom"]["request_max_retries"].as_integer(),
            Some(11)
        );
        assert_eq!(
            final_doc["desktop"]["followUpQueueMode"].as_str(),
            Some("queue")
        );
        assert_eq!(
            final_doc["desktop"]["localeOverride"].as_str(),
            Some("zh-CN")
        );
        assert_eq!(final_doc["features"]["js_repl"].as_bool(), Some(false));
        assert!(!final_config.contains("experimental_bearer_token"));

        let snapshot_path = providers::official_snapshot_path_for_test(&codex_dir)
            .expect("resolve official snapshot path");
        let _ = fs::remove_file(snapshot_path);
        let _ = fs::remove_dir_all(codex_dir);
    }
}

#[test]
fn saved_provider_a_to_official_to_saved_provider_b_uses_provider_b_auth() {
    let _db_guard = crate::app_db::test_db_guard();
    let codex_dir = temp_codex_dir("saved-provider-a-official-b");
    let suffix = Local::now().timestamp_nanos_opt().unwrap_or_default();
    let provider_a_id = format!("roundtrip-a-{suffix}");
    let provider_b_id = format!("roundtrip-b-{suffix}");
    let official_config = r#"model_provider = "openai"
model = "official-model"
"#;
    let official_auth = json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "access_token": "official-access-token",
            "refresh_token": "official-refresh-token"
        }
    });
    let provider_a_config = r#"model_provider = "custom"
model = "proxy-model-a"

[model_providers.custom]
name = "Proxy A"
base_url = "https://a.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#;
    let provider_b_config = r#"# keep-provider-b-template
model_provider = "custom"
model = "gpt-5.6-sol"
model_reasoning_effort = "xhigh"
service_tier = "priority"

[model_providers.custom]
name = "Proxy B"
base_url = "https://b.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
request_max_retries = 9

[desktop]
localeOverride = "zh-CN"

[plugins."browser@openai-bundled"]
enabled = true
"#;

    write_text(&config_path(&codex_dir), official_config).expect("write official config");
    write_json(&auth_path(&codex_dir), &official_auth).expect("write official auth");
    assert!(providers::capture_live_chatgpt_config(&codex_dir).expect("capture official snapshot"));

    for provider in [
        SavedProvider {
            id: provider_a_id.clone(),
            provider_name: "Proxy A".to_string(),
            base_url: "https://a.example.com/v1".to_string(),
            model: "proxy-model-a".to_string(),
            api_key: Some("sk-provider-a".to_string()),
            toml_config: Some(provider_a_config.to_string()),
            wire_api: "responses".to_string(),
            requires_openai_auth: true,
        },
        SavedProvider {
            id: provider_b_id.clone(),
            provider_name: "Proxy B".to_string(),
            base_url: "https://b.example.com/v1".to_string(),
            model: "gpt-5.6-sol".to_string(),
            api_key: Some("sk-provider-b".to_string()),
            toml_config: Some(provider_b_config.to_string()),
            wire_api: "responses".to_string(),
            requires_openai_auth: true,
        },
    ] {
        save_provider_inner(provider).expect("save provider record");
    }

    save_provider_toml_config_inner(ProviderTomlInput {
        config_dir: Some(codex_dir.display().to_string()),
        config_text: provider_a_config.to_string(),
        api_key: Some("sk-provider-a".to_string()),
    })
    .expect("activate provider A");
    write_json(
        &auth_path(&codex_dir),
        &json!({"OPENAI_API_KEY": "sk-provider-a", "auth_mode": "apikey"}),
    )
    .expect("simulate legacy provider auth shape");

    let official = switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("switch provider A to official");
    assert!(official.state.is_official_provider);
    assert_eq!(
        serde_json::from_slice::<Value>(
            &fs::read(auth_path(&codex_dir)).expect("read restored official auth")
        )
        .expect("parse restored official auth"),
        official_auth
    );

    let provider_b = list_saved_providers_inner()
        .expect("list saved providers")
        .into_iter()
        .find(|provider| provider.id == provider_b_id)
        .expect("find saved provider B");
    let switched = save_provider_toml_config_inner(ProviderTomlInput {
        config_dir: Some(codex_dir.display().to_string()),
        config_text: provider_b.toml_config.expect("provider B complete TOML"),
        api_key: provider_b.api_key,
    })
    .expect("switch official to saved provider B");

    assert_eq!(switched.message, "已切换到 Proxy B");
    assert!(!switched.state.is_official_provider);
    assert_eq!(switched.state.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(
        serde_json::from_slice::<Value>(
            &fs::read(auth_path(&codex_dir)).expect("read final provider B auth")
        )
        .expect("parse final provider B auth"),
        json!({"OPENAI_API_KEY": "sk-provider-b"})
    );
    let final_config = fs::read_to_string(config_path(&codex_dir)).expect("read provider B config");
    let final_doc = final_config
        .parse::<DocumentMut>()
        .expect("parse provider B config");
    assert!(final_config.contains("# keep-provider-b-template"));
    assert_eq!(final_doc["model"].as_str(), Some("gpt-5.6-sol"));
    assert_eq!(final_doc["model_reasoning_effort"].as_str(), Some("xhigh"));
    assert_eq!(
        final_doc["model_providers"]["custom"]["base_url"].as_str(),
        Some("https://b.example.com/v1")
    );
    assert!(final_doc["model_providers"]["custom"]
        .get("experimental_bearer_token")
        .is_none());
    assert_eq!(
        final_doc["plugins"]["browser@openai-bundled"]["enabled"].as_bool(),
        Some(true)
    );

    providers::delete_provider_inner(&provider_a_id).expect("delete provider A");
    providers::delete_provider_inner(&provider_b_id).expect("delete provider B");
    let snapshot_path = providers::official_snapshot_path_for_test(&codex_dir)
        .expect("resolve official snapshot path");
    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn switch_provider_round_trip_restores_official_api_key() {
    let codex_dir = temp_codex_dir("switch-provider-official-api-key-round-trip");
    write_text(
        &config_path(&codex_dir),
        "model_provider = \"openai\"\nmodel = \"official-model\"\n",
    )
    .expect("write official config");
    let official_auth = json!({
        "OPENAI_API_KEY": "sk-official-api-key",
        "auth_mode": "apikey"
    });
    write_json(&auth_path(&codex_dir), &official_auth).expect("write official API-key auth");
    let snapshot_path = providers::official_snapshot_path_for_test(&codex_dir)
        .expect("resolve official snapshot path");
    let _ = fs::remove_file(&snapshot_path);

    assert!(
        !providers::capture_live_chatgpt_config(&codex_dir).expect("refresh official auth state")
    );
    assert!(!snapshot_path.exists());

    switch_provider_inner(ProviderInput {
        config_dir: Some(codex_dir.display().to_string()),
        provider_id: Some("proxy".to_string()),
        provider_name: "Proxy".to_string(),
        base_url: "https://proxy.example.com/v1".to_string(),
        model: "proxy-model".to_string(),
        api_key: Some("sk-proxy".to_string()),
        wire_api: Some("responses".to_string()),
        requires_openai_auth: Some(false),
    })
    .expect("switch to proxy");
    assert!(snapshot_path.is_file());
    let proxy_auth: Value =
        serde_json::from_str(&fs::read_to_string(auth_path(&codex_dir)).expect("read proxy auth"))
            .expect("parse proxy auth");
    assert_eq!(proxy_auth, json!({"OPENAI_API_KEY": "sk-proxy"}));

    let result = switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("switch back to official API-key config");
    assert!(result.state.is_official_provider);
    assert_eq!(result.state.model.as_deref(), Some("official-model"));
    let restored_auth: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read restored official API-key auth"),
    )
    .expect("parse restored official API-key auth");
    assert_eq!(restored_auth, official_auth);

    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn polluted_official_live_api_key_does_not_replace_saved_oauth() {
    let codex_dir = temp_codex_dir("polluted-official-live-auth");
    write_text(
        &config_path(&codex_dir),
        "model_provider = \"openai\"\nmodel = \"official-model\"\n",
    )
    .expect("write official config");
    let official_auth = json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "access_token": "official-access-token",
            "refresh_token": "official-refresh-token"
        }
    });
    write_json(&auth_path(&codex_dir), &official_auth).expect("write official OAuth");
    assert!(
        providers::capture_live_chatgpt_config(&codex_dir).expect("capture trusted OAuth snapshot")
    );

    // Older Codex-X builds could leave this mixed state after a switch.
    write_json(
        &auth_path(&codex_dir),
        &json!({"OPENAI_API_KEY": "sk-stale-proxy", "auth_mode": "apikey"}),
    )
    .expect("pollute live official auth");

    switch_provider_inner(ProviderInput {
        config_dir: Some(codex_dir.display().to_string()),
        provider_id: Some("proxy".to_string()),
        provider_name: "Proxy".to_string(),
        base_url: "https://proxy.example.com/v1".to_string(),
        model: "proxy-model".to_string(),
        api_key: Some("sk-proxy".to_string()),
        wire_api: Some("responses".to_string()),
        requires_openai_auth: Some(true),
    })
    .expect("switch away from polluted official state");

    switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("restore the trusted OAuth snapshot");
    assert_eq!(
        serde_json::from_str::<Value>(
            &fs::read_to_string(auth_path(&codex_dir)).expect("read restored OAuth")
        )
        .expect("parse restored OAuth"),
        official_auth
    );

    let snapshot_path = providers::official_snapshot_path_for_test(&codex_dir)
        .expect("resolve official snapshot path");
    let snapshot: Value = serde_json::from_str(
        &fs::read_to_string(&snapshot_path).expect("read repaired official snapshot"),
    )
    .expect("parse repaired official snapshot");
    assert_eq!(snapshot["auth"], official_auth);

    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn switch_provider_pre_switch_hook_observes_current_before_replacing_auth() {
    let codex_dir = temp_codex_dir("switch-provider-persist-current");
    write_text(
        &config_path(&codex_dir),
        r#"model_provider = "custom"
model = "model-a"
approval_policy = "never"

[model_providers.custom]
name = "Provider A"
base_url = "https://a.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-a-scoped"

[mcp_servers.docs]
command = "docs-server"
"#,
    )
    .expect("write provider A config");
    let official_auth = json!({
        "auth_mode": "chatgpt",
        "tokens": {"access_token": "official-access-token"}
    });
    write_json(&auth_path(&codex_dir), &official_auth).expect("write official auth");

    let persisted = std::cell::RefCell::new(None);
    let result = switch_provider_with_pre_persist(
        ProviderInput {
            config_dir: Some(codex_dir.display().to_string()),
            provider_id: Some("provider-b".to_string()),
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
    let provider_a_toml = provider_a.toml_config.as_deref().expect("provider A TOML");
    let provider_a_doc = provider_a_toml
        .parse::<DocumentMut>()
        .expect("parse provider A TOML");
    assert!(!provider_a_toml.contains("experimental_bearer_token"));
    assert_eq!(provider_a_doc["approval_policy"].as_str(), Some("never"));
    assert_eq!(
        provider_a_doc["mcp_servers"]["docs"]["command"].as_str(),
        Some("docs-server")
    );
    assert_eq!(result.state.model.as_deref(), Some("model-b"));
    assert!(result
        .state
        .config_text
        .contains("https://b.example.com/v1"));
    assert!(!result
        .state
        .config_text
        .contains("https://a.example.com/v1"));
    assert!(result
        .state
        .config_text
        .contains("approval_policy = \"never\""));
    assert!(result.state.config_text.contains("[mcp_servers.docs]"));
    let live_auth: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read provider B auth"),
    )
    .expect("parse provider B auth");
    assert_eq!(live_auth, json!({"OPENAI_API_KEY": "sk-b"}));
    let live_doc = result
        .state
        .config_text
        .parse::<DocumentMut>()
        .expect("parse provider B live config");
    assert!(live_doc["model_providers"]["custom"]
        .get("experimental_bearer_token")
        .is_none());

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn switch_provider_reserved_builtin_ids_still_write_live_custom() {
    let codex_dir = temp_codex_dir("switch-provider-reserved");
    let result = switch_provider_inner(ProviderInput {
        config_dir: Some(codex_dir.display().to_string()),
        provider_id: Some("openai".to_string()),
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
    let config_doc = config_text
        .parse::<DocumentMut>()
        .expect("parse reserved-id provider config");
    assert!(config_doc["model_providers"]["custom"]
        .get("experimental_bearer_token")
        .is_none());
    let provider_auth: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read provider auth"),
    )
    .expect("parse provider auth");
    assert_eq!(provider_auth, json!({"OPENAI_API_KEY": "sk-proxy"}));

    let official = switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("enter official login without an existing official account");
    assert!(official.state.is_official_provider);
    assert!(!auth_path(&codex_dir).exists());

    switch_provider_inner(ProviderInput {
        config_dir: Some(codex_dir.display().to_string()),
        provider_id: Some("openai".to_string()),
        provider_name: "OpenAI".to_string(),
        base_url: "https://proxy.example.com/v1".to_string(),
        model: "gpt-5.5".to_string(),
        api_key: Some("sk-proxy".to_string()),
        wire_api: Some("responses".to_string()),
        requires_openai_auth: None,
    })
    .expect("switch to provider again after clean official state");
    let provider_auth_again: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read provider auth again"),
    )
    .expect("parse provider auth again");
    assert_eq!(provider_auth_again, json!({"OPENAI_API_KEY": "sk-proxy"}));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn switch_official_pre_switch_hook_observes_custom_before_overwrite() {
    let codex_dir = temp_codex_dir("switch-official-persist-current");
    write_text(
        &config_path(&codex_dir),
        "model_provider = \"openai\"\nmodel = \"official-model\"\n",
    )
    .expect("write original official config");
    write_json(
        &auth_path(&codex_dir),
        &json!({
            "auth_mode": "chatgpt",
            "tokens": {"access_token": "official-access-token"}
        }),
    )
    .expect("write original official auth");
    create_backup(&codex_dir, "seed-official").expect("backup official config");
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
    assert!(result.state.is_official_provider);
    assert_eq!(result.state.model.as_deref(), Some("official-model"));
    assert!(!result
        .state
        .config_text
        .contains("https://a.example.com/v1"));
    let restored_auth: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read restored auth"),
    )
    .expect("parse restored auth");
    assert_eq!(
        restored_auth["tokens"]["access_token"].as_str(),
        Some("official-access-token")
    );

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn switch_official_without_snapshot_clears_third_party_auth_for_login() {
    let codex_dir = temp_codex_dir("switch-official-preserve-auth");
    write_text(
        &config_path(&codex_dir),
        r#"model_provider = "custom"
model = "gpt-5.5"
base_url = "https://legacy-proxy.example.com/v1"

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

    let result = switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("switch to a clean official login state");
    assert_eq!(result.state.model_provider.as_deref(), Some("custom"));
    assert!(result.state.is_official_provider);
    assert!(!auth_path(&codex_dir).exists());
    let config_text = fs::read_to_string(config_path(&codex_dir)).expect("read config");
    assert!(config_text.contains("model_provider = \"custom\""));
    assert!(!config_text.contains("base_url"));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn switch_official_keeps_api_key_auth_when_live_route_is_already_official() {
    let codex_dir = temp_codex_dir("switch-official-keep-official-api-key");
    write_text(
        &config_path(&codex_dir),
        "model_provider = \"openai\"\nmodel = \"gpt-5.5\"\n",
    )
    .expect("write official config");
    let official_auth = json!({
        "OPENAI_API_KEY": "sk-official-api-key",
        "auth_mode": "apikey"
    });
    write_json(&auth_path(&codex_dir), &official_auth).expect("write official API-key auth");

    let result = switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("refresh already-official route");

    assert!(result.state.is_official_provider);
    let retained: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read retained auth"),
    )
    .expect("parse retained auth");
    assert_eq!(retained, official_auth);

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn switch_official_ignores_api_key_only_official_history() {
    let codex_dir = temp_codex_dir("switch-official-reject-polluted-history");
    write_text(
        &config_path(&codex_dir),
        "model_provider = \"openai\"\nmodel = \"gpt-5.5\"\n",
    )
    .expect("write apparently official config");
    write_json(
        &auth_path(&codex_dir),
        &json!({"OPENAI_API_KEY": "sk-proxy-in-old-backup", "auth_mode": "apikey"}),
    )
    .expect("write polluted official auth");
    create_backup(&codex_dir, "old-broken-switch-official").expect("backup polluted state");

    write_text(
        &config_path(&codex_dir),
        r#"model_provider = "custom"
model = "proxy-model"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#,
    )
    .expect("write custom config");

    let result = switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("switch without restoring polluted auth");
    assert_eq!(result.state.model_provider.as_deref(), Some("custom"));
    assert!(result.state.is_official_provider);
    assert!(!auth_path(&codex_dir).exists());
    assert!(fs::read_to_string(config_path(&codex_dir))
        .expect("read official config")
        .contains("model_provider = \"custom\""));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn switch_provider_snapshot_restores_auth_after_external_cc_overwrite() {
    let codex_dir = temp_codex_dir("switch-provider-official-snapshot");
    write_text(
        &config_path(&codex_dir),
        "model_provider = \"openai\"\nmodel = \"official-model\"\n",
    )
    .expect("write official config");
    let official_auth = json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "access_token": "official-access-token",
            "refresh_token": "official-refresh-token"
        }
    });
    write_json(&auth_path(&codex_dir), &official_auth).expect("write official auth");

    switch_provider_inner(ProviderInput {
        config_dir: Some(codex_dir.display().to_string()),
        provider_id: Some("proxy".to_string()),
        provider_name: "Proxy".to_string(),
        base_url: "https://proxy.example.com/v1".to_string(),
        model: "proxy-model".to_string(),
        api_key: Some("sk-proxy".to_string()),
        wire_api: Some("responses".to_string()),
        requires_openai_auth: Some(true),
    })
    .expect("switch to proxy");
    assert!(providers::official_snapshot_path_for_test(&codex_dir)
        .expect("snapshot path")
        .is_file());

    write_json(
        &auth_path(&codex_dir),
        &json!({"OPENAI_API_KEY": "sk-cc-overwrite", "auth_mode": "apikey"}),
    )
    .expect("simulate cc-switch auth overwrite");

    let result = switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("restore official snapshot");
    assert_eq!(result.state.model_provider.as_deref(), Some("openai"));
    assert!(result.state.is_official_provider);
    assert_eq!(result.state.model.as_deref(), Some("official-model"));
    let restored: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read restored auth"),
    )
    .expect("parse restored auth");
    assert_eq!(restored, official_auth);

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn save_official_config_while_custom_does_not_touch_live_files() {
    let codex_dir = temp_codex_dir("save-official-while-custom");
    let custom_config = r#"model_provider = "custom"
model = "proxy-model"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#;
    let proxy_auth = json!({"OPENAI_API_KEY": "sk-proxy", "auth_mode": "apikey"});
    write_text(&config_path(&codex_dir), custom_config).expect("write custom config");
    write_json(&auth_path(&codex_dir), &proxy_auth).expect("write proxy auth");

    save_official_config_inner(
        Some(codex_dir.display().to_string()),
        Some("official-model".to_string()),
        Some(
            json!({
                "auth_mode": "chatgpt",
                "tokens": {"access_token": "official-access-token"}
            })
            .to_string(),
        ),
        None,
    )
    .expect("save independent official config");

    assert_eq!(
        fs::read_to_string(config_path(&codex_dir)).expect("read live config"),
        custom_config
    );
    let live_auth: Value =
        serde_json::from_str(&fs::read_to_string(auth_path(&codex_dir)).expect("read live auth"))
            .expect("parse live auth");
    assert_eq!(live_auth, proxy_auth);

    let switched = switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("switch to saved official config");
    assert_eq!(switched.state.model.as_deref(), Some("official-model"));
    assert_eq!(switched.state.model_provider.as_deref(), Some("custom"));
    assert!(switched.state.is_official_provider);
    let restored: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read official auth"),
    )
    .expect("parse official auth");
    assert_eq!(
        restored["tokens"]["access_token"].as_str(),
        Some("official-access-token")
    );

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn complete_official_toml_model_is_authoritative() {
    let codex_dir = temp_codex_dir("official-toml-model-authority");
    let official_config = r#"model_provider = "openai"
model = "toml-model"
model_reasoning_effort = "xhigh"

[features]
js_repl = false
"#;
    let result = save_official_config_inner(
        Some(codex_dir.display().to_string()),
        Some("stale-form-model".to_string()),
        Some(
            json!({
                "auth_mode": "chatgpt",
                "tokens": {"access_token": "official-access-token"}
            })
            .to_string(),
        ),
        Some(official_config.to_string()),
    )
    .expect("save complete official TOML");

    assert_eq!(result.state.model.as_deref(), Some("toml-model"));
    let saved = fs::read_to_string(config_path(&codex_dir)).expect("read saved official TOML");
    assert!(saved.contains("model = \"toml-model\""));
    assert!(!saved.contains("stale-form-model"));
    assert!(saved.contains("model_reasoning_effort = \"xhigh\""));
    assert!(saved.contains("[features]"));

    let snapshot_path = providers::official_snapshot_path_for_test(&codex_dir)
        .expect("resolve official snapshot path");
    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn complete_official_config_round_trips_with_auth_snapshot() {
    let codex_dir = temp_codex_dir("complete-official-config-round-trip");
    let proxy_config = r#"model_provider = "custom"
model = "proxy-model"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#;
    let official_config = r#"# keep-official-comment
model_provider = "openai"
model = "official-model"
model_reasoning_effort = "xhigh"
disable_response_storage = true
approval_policy = "on-request"
notify = ["C:\\Tools\\codex-notify.exe", "turn-ended"]

[desktop]
localeOverride = "zh-CN"
integratedTerminalShell = "gitBash"

[windows]
sandbox = "elevated"
shell_path = 'D:\Tools\PowerShell\pwsh.exe'

[features]
js_repl = false

[projects.'C:\Users\Tester\Documents\Codex\official-project']
trust_level = "trusted"

[plugins."browser@openai-bundled"]
enabled = true

[marketplaces.openai-bundled]
last_updated = "2026-08-11T04:59:25Z"
source_type = "local"
source = '\\?\C:\Users\Tester\.codex\.tmp\bundled-marketplaces\openai-bundled'

[shell_environment_policy.set]
BROWSER_USE_AVAILABLE_BACKENDS = "chrome,iab"
CODEX_HOME = 'C:\Users\Tester\.codex'

[mcp_servers.docs]
command = "docs-server"
"#;
    let proxy_auth = json!({"OPENAI_API_KEY": "sk-proxy"});
    let official_auth = json!({
        "auth_mode": "chatgpt",
        "tokens": {"access_token": "official-access-token"}
    });
    write_text(&config_path(&codex_dir), proxy_config).expect("write proxy config");
    write_json(&auth_path(&codex_dir), &proxy_auth).expect("write proxy auth");

    save_official_config_inner(
        Some(codex_dir.display().to_string()),
        Some("official-model".to_string()),
        Some(official_auth.to_string()),
        Some(official_config.to_string()),
    )
    .expect("save complete official snapshot");

    assert_eq!(
        fs::read_to_string(config_path(&codex_dir)).expect("read unchanged proxy config"),
        proxy_config
    );
    assert_eq!(
        serde_json::from_str::<Value>(
            &fs::read_to_string(auth_path(&codex_dir)).expect("read unchanged proxy auth")
        )
        .expect("parse proxy auth"),
        proxy_auth
    );

    let switched = switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("switch to complete official snapshot");
    assert!(switched.state.is_official_provider);
    let restored = fs::read_to_string(config_path(&codex_dir)).expect("read official config");
    assert!(restored.contains("# keep-official-comment"));
    assert!(restored.contains("[desktop]"));
    assert!(restored.contains("[windows]"));
    assert!(restored.contains("[features]"));
    assert!(restored.contains("[projects.'C:\\Users\\Tester\\Documents\\Codex\\official-project']"));
    assert!(restored.contains("[plugins.\"browser@openai-bundled\"]"));
    assert!(restored.contains("[marketplaces.openai-bundled]"));
    assert!(restored.contains("[shell_environment_policy.set]"));
    assert!(restored.contains("[mcp_servers.docs]"));
    assert!(!restored.contains("proxy.example.com"));
    assert_eq!(
        serde_json::from_str::<Value>(
            &fs::read_to_string(auth_path(&codex_dir)).expect("read official auth")
        )
        .expect("parse official auth"),
        official_auth
    );

    let draft = get_official_config_draft_inner(Some(codex_dir.display().to_string()))
        .expect("read complete official draft")
        .expect("official draft");
    let draft = serde_json::to_value(draft).expect("serialize official draft");
    assert!(draft["configText"]
        .as_str()
        .is_some_and(|text| text.contains("# keep-official-comment")));

    let snapshot_path = providers::official_snapshot_path_for_test(&codex_dir)
        .expect("resolve official snapshot path");
    let snapshot: Value =
        serde_json::from_str(&fs::read_to_string(&snapshot_path).expect("read official snapshot"))
            .expect("parse official snapshot");
    assert_eq!(snapshot["version"].as_u64(), Some(3));
    assert!(snapshot["config"]
        .as_str()
        .is_some_and(|text| text.contains("[desktop]")));

    let switch_to_proxy = || {
        switch_provider_inner(ProviderInput {
            config_dir: Some(codex_dir.display().to_string()),
            provider_id: Some("proxy".to_string()),
            provider_name: "Proxy".to_string(),
            base_url: "https://proxy.example.com/v1".to_string(),
            model: "proxy-model".to_string(),
            api_key: Some("sk-proxy".to_string()),
            wire_api: Some("responses".to_string()),
            requires_openai_auth: Some(true),
        })
    };
    switch_to_proxy().expect("switch from complete official config to proxy");
    assert_eq!(
        serde_json::from_str::<Value>(
            &fs::read_to_string(auth_path(&codex_dir)).expect("read proxy auth")
        )
        .expect("parse proxy auth"),
        json!({"OPENAI_API_KEY": "sk-proxy"})
    );

    switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("restore complete official config again");
    let restored_again =
        fs::read_to_string(config_path(&codex_dir)).expect("read official config again");
    for expected in [
        "notify =",
        "[desktop]",
        "localeOverride = \"zh-CN\"",
        "[windows]",
        "[features]",
        "[plugins.\"browser@openai-bundled\"]",
        "[marketplaces.openai-bundled]",
        "[shell_environment_policy.set]",
        "[mcp_servers.docs]",
    ] {
        assert!(restored_again.contains(expected), "missing {expected}");
    }

    switch_to_proxy().expect("switch to proxy after second official activation");
    let final_proxy = fs::read_to_string(config_path(&codex_dir)).expect("read final proxy config");
    let final_proxy = final_proxy
        .parse::<DocumentMut>()
        .expect("parse final proxy config");
    assert!(final_proxy["model_providers"]["custom"]
        .get("experimental_bearer_token")
        .is_none());
    assert_eq!(
        serde_json::from_str::<Value>(
            &fs::read_to_string(auth_path(&codex_dir)).expect("read final proxy auth")
        )
        .expect("parse final proxy auth"),
        json!({"OPENAI_API_KEY": "sk-proxy"})
    );

    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn complete_official_config_without_auth_survives_provider_round_trip() {
    let codex_dir = temp_codex_dir("complete-official-config-without-auth");
    let official_config = r#"model_provider = "openai"
model = "official-model"
notify = ["C:\\Tools\\notify.exe", "turn-ended"]

[desktop]
localeOverride = "zh-CN"

[windows]
sandbox = "elevated"

[plugins."browser@openai-bundled"]
enabled = true
"#;
    write_text(&config_path(&codex_dir), official_config).expect("write official config");

    switch_provider_inner(ProviderInput {
        config_dir: Some(codex_dir.display().to_string()),
        provider_id: Some("proxy".to_string()),
        provider_name: "Proxy".to_string(),
        base_url: "https://proxy.example.com/v1".to_string(),
        model: "proxy-model".to_string(),
        api_key: Some("sk-proxy".to_string()),
        wire_api: Some("responses".to_string()),
        requires_openai_auth: Some(true),
    })
    .expect("switch to proxy without official auth");

    switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("restore config-only official snapshot");
    let restored = fs::read_to_string(config_path(&codex_dir)).expect("read restored config");
    assert!(restored.contains("notify ="));
    assert!(restored.contains("[desktop]"));
    assert!(restored.contains("localeOverride = \"zh-CN\""));
    assert!(restored.contains("[windows]"));
    assert!(restored.contains("[plugins.\"browser@openai-bundled\"]"));
    assert!(!restored.contains("proxy.example.com"));
    assert!(!auth_path(&codex_dir).exists());

    let snapshot_path = providers::official_snapshot_path_for_test(&codex_dir)
        .expect("resolve config-only official snapshot path");
    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn production_switch_backfills_auth_only_provider_before_round_trip() {
    let _db_guard = crate::app_db::test_db_guard();
    let codex_dir = temp_codex_dir("production-provider-backfill");
    let provider_id = format!(
        "backfill-{}",
        Local::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let provider_config = r#"model_provider = "custom"
model = "proxy-model"
approval_policy = "never"

[model_providers.custom]
name = "Backfill Proxy"
base_url = "https://backfill.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#;
    save_provider_inner(SavedProvider {
        id: provider_id.clone(),
        provider_name: "Backfill Proxy".to_string(),
        base_url: "https://backfill.example.com/v1".to_string(),
        model: "proxy-model".to_string(),
        api_key: None,
        toml_config: Some(provider_config.to_string()),
        wire_api: "responses".to_string(),
        requires_openai_auth: true,
    })
    .expect("save provider without stored key");
    write_text(&config_path(&codex_dir), provider_config).expect("write live provider config");
    write_json(
        &auth_path(&codex_dir),
        &json!({"OPENAI_API_KEY": "sk-auth-only"}),
    )
    .expect("write auth-only provider key");

    switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("switch to official after backfill");
    let saved = list_saved_providers_inner()
        .expect("list saved providers")
        .into_iter()
        .find(|provider| provider.id == provider_id)
        .expect("backfilled provider");
    assert_eq!(saved.api_key.as_deref(), Some("sk-auth-only"));
    assert!(saved
        .toml_config
        .as_deref()
        .is_some_and(|text| text.contains("approval_policy = \"never\"")));

    save_provider_toml_config_inner(ProviderTomlInput {
        config_dir: Some(codex_dir.display().to_string()),
        config_text: saved
            .toml_config
            .clone()
            .expect("saved full provider config"),
        api_key: saved.api_key.clone(),
    })
    .expect("switch back to backfilled provider");
    assert_eq!(
        serde_json::from_str::<Value>(
            &fs::read_to_string(auth_path(&codex_dir)).expect("read restored provider auth")
        )
        .expect("parse restored provider auth"),
        json!({"OPENAI_API_KEY": "sk-auth-only"})
    );
    let restored_provider = fs::read_to_string(config_path(&codex_dir))
        .expect("read restored provider config")
        .parse::<DocumentMut>()
        .expect("parse restored provider config");
    assert!(restored_provider["model_providers"]["custom"]
        .get("experimental_bearer_token")
        .is_none());

    crate::providers::delete_provider_inner(&provider_id).expect("delete backfill test provider");
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn legacy_v2_official_snapshot_is_upgraded_with_complete_config() {
    let codex_dir = temp_codex_dir("legacy-v2-official-snapshot");
    write_text(
        &config_path(&codex_dir),
        r#"model_provider = "custom"
model = "proxy-model"
approval_policy = "never"

[model_providers.custom]
name = "Proxy"
base_url = "https://legacy-proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#,
    )
    .expect("write live proxy config");
    write_json(
        &auth_path(&codex_dir),
        &json!({"OPENAI_API_KEY": "sk-proxy"}),
    )
    .expect("write live proxy auth");

    let snapshot_path = providers::official_snapshot_path_for_test(&codex_dir)
        .expect("resolve legacy snapshot path");
    fs::create_dir_all(snapshot_path.parent().expect("snapshot parent"))
        .expect("create snapshot parent");
    let canonical_dir = fs::canonicalize(&codex_dir)
        .expect("canonicalize Codex dir")
        .to_string_lossy()
        .to_string();
    write_json(
        &snapshot_path,
        &json!({
            "version": 2,
            "codexDir": canonical_dir,
            "capturedAt": "2026-08-11T00:00:00+08:00",
            "model": "legacy-official-model",
            "auth": {
                "auth_mode": "chatgpt",
                "tokens": {"access_token": "legacy-official-token"}
            }
        }),
    )
    .expect("write legacy v2 snapshot");

    let result = switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("restore legacy official snapshot");
    assert!(result.state.is_official_provider);
    assert_eq!(result.state.model.as_deref(), Some("legacy-official-model"));
    assert!(result
        .state
        .config_text
        .contains("approval_policy = \"never\""));
    assert!(!result
        .state
        .config_text
        .contains("legacy-proxy.example.com"));
    let upgraded: Value =
        serde_json::from_str(&fs::read_to_string(&snapshot_path).expect("read upgraded snapshot"))
            .expect("parse upgraded snapshot");
    assert_eq!(upgraded["version"].as_u64(), Some(3));
    assert!(upgraded["config"]
        .as_str()
        .is_some_and(|text| text.contains("approval_policy = \"never\"")));

    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn state_reports_saved_official_auth_without_live_auth_file() {
    let codex_dir = temp_codex_dir("state-saved-official-auth");
    write_text(
        &config_path(&codex_dir),
        r#"model_provider = "custom"
model = "proxy-model"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-proxy"
"#,
    )
    .expect("write custom config");

    let result = save_official_config_inner(
        Some(codex_dir.display().to_string()),
        Some("official-model".to_string()),
        Some(
            json!({
                "auth_mode": "chatgpt",
                "tokens": {"access_token": "official-access-token"}
            })
            .to_string(),
        ),
        None,
    )
    .expect("save independent official config");

    assert!(!auth_path(&codex_dir).exists());
    let state = serde_json::to_value(result.state).expect("serialize state");
    assert_eq!(state["authExists"].as_bool(), Some(false));
    assert_eq!(state["officialAuthAvailable"].as_bool(), Some(true));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn state_reports_live_chatgpt_auth_while_a_third_party_provider_is_active() {
    let codex_dir = temp_codex_dir("state-third-party-with-official-auth");
    write_text(
        &config_path(&codex_dir),
        r#"model_provider = "custom"
model = "proxy-model"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "sk-proxy"
"#,
    )
    .expect("write provider config");
    write_json(
        &auth_path(&codex_dir),
        &json!({
            "auth_mode": "chatgpt",
            "tokens": {"access_token": "official-access-token"}
        }),
    )
    .expect("write official auth");

    let state = build_state(codex_dir.clone()).expect("build state");
    let state = serde_json::to_value(state).expect("serialize state");
    assert_eq!(state["authExists"].as_bool(), Some(true));
    assert_eq!(state["officialAuthAvailable"].as_bool(), Some(true));
    assert_eq!(state["isOfficialProvider"].as_bool(), Some(false));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn malformed_official_snapshot_does_not_block_state_or_provider_switch() {
    let codex_dir = temp_codex_dir("malformed-official-snapshot");
    write_text(
        &config_path(&codex_dir),
        "model_provider = \"openai\"\nmodel = \"official-model\"\n",
    )
    .expect("write official config");
    let official_auth = json!({
        "auth_mode": "chatgpt",
        "tokens": {"access_token": "official-access-token"}
    });
    write_json(&auth_path(&codex_dir), &official_auth).expect("write official auth");
    let snapshot_path = providers::official_snapshot_path_for_test(&codex_dir)
        .expect("resolve official snapshot path");
    fs::create_dir_all(snapshot_path.parent().expect("snapshot parent"))
        .expect("create snapshot parent");
    write_text(&snapshot_path, "{truncated-snapshot").expect("write malformed official snapshot");

    let state = serde_json::to_value(build_state(codex_dir.clone()).expect("build state"))
        .expect("serialize state");
    assert_eq!(state["officialAuthAvailable"].as_bool(), Some(true));
    assert_eq!(state["isOfficialProvider"].as_bool(), Some(true));

    switch_provider_inner(ProviderInput {
        config_dir: Some(codex_dir.display().to_string()),
        provider_id: Some("proxy".to_string()),
        provider_name: "Proxy".to_string(),
        base_url: "https://proxy.example.com/v1".to_string(),
        model: "proxy-model".to_string(),
        api_key: Some("sk-proxy".to_string()),
        wire_api: Some("responses".to_string()),
        requires_openai_auth: Some(true),
    })
    .expect("switch provider and repair snapshot");

    let repaired: Value = serde_json::from_str(
        &fs::read_to_string(&snapshot_path).expect("read repaired official snapshot"),
    )
    .expect("parse repaired official snapshot");
    assert_eq!(repaired["auth"], official_auth);
    assert_eq!(
        serde_json::from_str::<Value>(
            &fs::read_to_string(auth_path(&codex_dir)).expect("read provider live auth")
        )
        .expect("parse provider live auth"),
        json!({"OPENAI_API_KEY": "sk-proxy"})
    );

    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn state_refresh_is_strictly_read_only() {
    let codex_dir = temp_codex_dir("state-refresh-read-only");
    write_text(
        &config_path(&codex_dir),
        "# preserve-state-bytes\nmodel_provider = \"openai\"\nmodel = \"gpt-5.5\"\n",
    )
    .expect("write config");
    write_json(
        &auth_path(&codex_dir),
        &json!({
            "auth_mode": "chatgpt",
            "tokens": {"access_token": "official-access"}
        }),
    )
    .expect("write official auth");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(config_path(&codex_dir), fs::Permissions::from_mode(0o644))
            .expect("set config test permissions");
        fs::set_permissions(auth_path(&codex_dir), fs::Permissions::from_mode(0o640))
            .expect("set auth test permissions");
    }
    let snapshot = official_snapshot_path_for_test(&codex_dir).expect("resolve snapshot path");
    let _ = fs::remove_file(&snapshot);
    let config_before = fs::read(config_path(&codex_dir)).expect("snapshot config bytes");
    let auth_before = fs::read(auth_path(&codex_dir)).expect("snapshot auth bytes");
    assert!(!snapshot.exists());

    let state = get_codex_state_inner(Some(codex_dir.display().to_string()))
        .expect("read Codex state without writes");

    assert!(state.is_official_provider);
    assert_eq!(state.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(
        fs::read(config_path(&codex_dir)).expect("read unchanged config"),
        config_before
    );
    assert_eq!(
        fs::read(auth_path(&codex_dir)).expect("read unchanged auth"),
        auth_before
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(config_path(&codex_dir))
                .expect("read config metadata")
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        assert_eq!(
            fs::metadata(auth_path(&codex_dir))
                .expect("read auth metadata")
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }
    assert!(!snapshot.exists());

    let _ = fs::remove_dir_all(codex_dir);
}

#[cfg(unix)]
#[test]
fn state_refresh_does_not_scan_historical_backups() {
    use std::os::unix::fs::PermissionsExt;

    let codex_dir = temp_codex_dir("state-refresh-skips-backups");
    write_text(
        &config_path(&codex_dir),
        r#"model_provider = "custom"
model = "proxy-model"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = false
"#,
    )
    .expect("write custom config");
    let history = codex_dir.join(".codexx-test-backups");
    fs::create_dir_all(&history).expect("create historical backup directory");
    fs::set_permissions(&history, fs::Permissions::from_mode(0o000))
        .expect("make historical backup directory unreadable");

    let result = get_codex_state_inner(Some(codex_dir.display().to_string()));
    fs::set_permissions(&history, fs::Permissions::from_mode(0o700))
        .expect("restore historical backup permissions");
    let state = result.expect("core state must not traverse historical backups");
    assert!(!state.is_official_provider);
    assert_eq!(state.model.as_deref(), Some("proxy-model"));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn active_remote_prompt_lookup_does_not_migrate_legacy_config() {
    let codex_dir = temp_codex_dir("active-remote-prompt-read-only");
    write_text(
        &config_path(&codex_dir),
        "# keep-legacy-layout\n[tui]\nmodel_instructions_file = \"./remote.md\"\n",
    )
    .expect("write legacy prompt config");
    let config_before = fs::read(config_path(&codex_dir)).expect("snapshot legacy config");

    assert_eq!(
        active_remote_builtin_prompt_id(Some(codex_dir.display().to_string())),
        None
    );

    assert_eq!(
        fs::read(config_path(&codex_dir)).expect("read unchanged legacy config"),
        config_before
    );
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn restore_official_config_while_custom_does_not_switch_live_provider() {
    let codex_dir = temp_codex_dir("restore-official-without-switching");
    write_text(
        &config_path(&codex_dir),
        "model_provider = \"openai\"\nmodel = \"official-model\"\n",
    )
    .expect("write official config");
    write_json(
        &auth_path(&codex_dir),
        &json!({
            "auth_mode": "chatgpt",
            "tokens": {"access_token": "official-access-token"}
        }),
    )
    .expect("write official auth");
    create_backup(&codex_dir, "seed-official").expect("backup official config");

    let custom_config = r#"model_provider = "custom"
model = "proxy-model"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#;
    let proxy_auth = json!({"OPENAI_API_KEY": "sk-proxy", "auth_mode": "apikey"});
    write_text(&config_path(&codex_dir), custom_config).expect("write custom config");
    write_json(&auth_path(&codex_dir), &proxy_auth).expect("write proxy auth");

    let result = restore_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("restore independent official config");

    assert_eq!(result.message, "已还原 OpenAI Official 配置");
    assert_eq!(result.state.model_provider.as_deref(), Some("custom"));
    assert_eq!(result.state.model.as_deref(), Some("proxy-model"));
    assert!(result.backup_id.is_none());
    assert_eq!(
        fs::read_to_string(config_path(&codex_dir)).expect("read unchanged config"),
        custom_config
    );
    let live_auth: Value =
        serde_json::from_str(&fs::read_to_string(auth_path(&codex_dir)).expect("read live auth"))
            .expect("parse live auth");
    assert_eq!(live_auth, proxy_auth);

    let snapshot_path = providers::official_snapshot_path_for_test(&codex_dir)
        .expect("resolve official snapshot path");
    let snapshot: Value = serde_json::from_str(
        &fs::read_to_string(snapshot_path).expect("read restored official snapshot"),
    )
    .expect("parse official snapshot");
    assert_eq!(snapshot["model"].as_str(), Some("official-model"));
    assert_eq!(
        snapshot["auth"]["tokens"]["access_token"].as_str(),
        Some("official-access-token")
    );

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn first_official_draft_from_proxy_preserves_complete_live_config() {
    let codex_dir = temp_codex_dir("first-official-draft-from-proxy");
    write_text(
        &config_path(&codex_dir),
        r#"model_provider = "custom"
model = "proxy-model"
approval_policy = "never"

[desktop]
localeOverride = "zh-CN"

[plugins."browser@openai-bundled"]
enabled = true

[features]
js_repl = false

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#,
    )
    .expect("write complete proxy config");
    write_json(
        &auth_path(&codex_dir),
        &json!({"OPENAI_API_KEY": "sk-proxy"}),
    )
    .expect("write proxy auth");

    let snapshot_path =
        providers::official_snapshot_path_for_test(&codex_dir).expect("resolve snapshot path");
    let _ = fs::remove_file(&snapshot_path);
    let draft = get_official_config_draft_inner(Some(codex_dir.display().to_string()))
        .expect("build official draft")
        .expect("official draft");
    let draft = serde_json::to_value(draft).expect("serialize official draft");
    let config = draft["configText"].as_str().expect("draft config text");

    assert!(draft["authJson"].as_str().is_some_and(str::is_empty));
    assert!(config.contains("[desktop]"));
    assert!(config.contains("[plugins.\"browser@openai-bundled\"]"));
    assert!(config.contains("[features]"));
    assert!(config.contains("name = \"OpenAI\""));
    assert!(!config.contains("proxy.example.com"));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn reset_official_config_clears_proxy_auth_and_requires_fresh_login() {
    let codex_dir = temp_codex_dir("reset-official-config");
    write_text(
        &config_path(&codex_dir),
        r#"model_provider = "custom"
model = "proxy-model"
approval_policy = "never"

[desktop]
localeOverride = "zh-CN"

[features]
js_repl = false

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#,
    )
    .expect("write custom config");
    write_json(
        &auth_path(&codex_dir),
        &json!({"OPENAI_API_KEY": "sk-proxy", "auth_mode": "apikey"}),
    )
    .expect("write proxy auth");

    let result = reset_official_provider_inner(
        Some(codex_dir.display().to_string()),
        Some("official-model".to_string()),
        None,
    )
    .expect("reset official config");

    assert_eq!(result.state.model_provider.as_deref(), Some("custom"));
    assert!(result.state.is_official_provider);
    assert_eq!(result.state.model.as_deref(), Some("official-model"));
    assert!(!auth_path(&codex_dir).exists());
    assert!(result
        .state
        .config_text
        .contains("[model_providers.custom]"));
    assert!(!result.state.config_text.contains("base_url"));
    assert!(result.state.config_text.contains("[desktop]"));
    assert!(result
        .state
        .config_text
        .contains("localeOverride = \"zh-CN\""));
    assert!(result.state.config_text.contains("[features]"));

    switch_provider_with_pre_persist(
        ProviderInput {
            config_dir: Some(codex_dir.display().to_string()),
            provider_id: Some("proxy".to_string()),
            provider_name: "Proxy".to_string(),
            base_url: "https://proxy.example.com/v1".to_string(),
            model: "proxy-model".to_string(),
            api_key: Some("sk-proxy".to_string()),
            wire_api: Some("responses".to_string()),
            requires_openai_auth: Some(true),
        },
        |_| Ok(()),
    )
    .expect("switch away from reset official config");
    assert!(auth_path(&codex_dir).is_file());

    let restored = switch_official_provider_with_pre_persist(
        Some(codex_dir.display().to_string()),
        |_| Ok(()),
    )
    .expect("switch back to reset official config");
    assert!(restored.state.is_official_provider);
    assert!(restored.state.config_text.contains("[desktop]"));
    assert!(restored.state.config_text.contains("[features]"));
    assert!(!auth_path(&codex_dir).exists());

    let snapshot_path =
        providers::official_snapshot_path_for_test(&codex_dir).expect("snapshot path");
    assert!(snapshot_path.is_file());

    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn reset_then_new_live_oauth_is_detected_and_preserved() {
    let codex_dir = temp_codex_dir("reset-then-new-live-oauth");
    write_text(
        &config_path(&codex_dir),
        "model_provider = \"openai\"\nmodel = \"official-model\"\n",
    )
    .expect("write official config");
    let previous_oauth = json!({
        "auth_mode": "chatgpt",
        "tokens": {"access_token": "previous-official-access"}
    });
    write_json(&auth_path(&codex_dir), &previous_oauth).expect("write previous official login");

    reset_official_provider_inner(
        Some(codex_dir.display().to_string()),
        Some("official-model".to_string()),
        None,
    )
    .expect("reset official config");

    let reset_draft = get_official_config_draft_inner(Some(codex_dir.display().to_string()))
        .expect("read reset draft")
        .expect("reset draft");
    let reset_draft = serde_json::to_value(reset_draft).expect("serialize reset draft");
    assert!(reset_draft["authJson"].as_str().is_some_and(str::is_empty));

    let fresh_oauth = json!({
        "OPENAI_API_KEY": null,
        "tokens": {"access_token": "fresh-official-access"},
        "last_refresh": "2026-08-11T00:00:00Z"
    });
    write_json(&auth_path(&codex_dir), &fresh_oauth).expect("write fresh official login");
    let state = serde_json::to_value(build_state(codex_dir.clone()).expect("refresh state"))
        .expect("serialize state");
    assert_eq!(state["officialAuthAvailable"].as_bool(), Some(true));
    let fresh_draft = get_official_config_draft_inner(Some(codex_dir.display().to_string()))
        .expect("read fresh login draft")
        .expect("fresh login draft");
    let fresh_draft = serde_json::to_value(fresh_draft).expect("serialize fresh login draft");
    let fresh_draft_auth: Value = serde_json::from_str(
        fresh_draft["authJson"]
            .as_str()
            .expect("fresh draft auth JSON"),
    )
    .expect("parse fresh draft auth JSON");
    assert_eq!(fresh_draft_auth, fresh_oauth);

    let switch_to_proxy = || {
        switch_provider_with_pre_persist(
            ProviderInput {
                config_dir: Some(codex_dir.display().to_string()),
                provider_id: Some("proxy".to_string()),
                provider_name: "Proxy".to_string(),
                base_url: "https://proxy.example.com/v1".to_string(),
                model: "proxy-model".to_string(),
                api_key: Some("sk-proxy".to_string()),
                wire_api: Some("responses".to_string()),
                requires_openai_auth: Some(true),
            },
            |_| Ok(()),
        )
    };

    switch_to_proxy().expect("switch fresh official login to proxy");
    let proxy_auth: Value =
        serde_json::from_str(&fs::read_to_string(auth_path(&codex_dir)).expect("read proxy auth"))
            .expect("parse proxy auth");
    assert_eq!(proxy_auth, json!({"OPENAI_API_KEY": "sk-proxy"}));
    let proxy_config = fs::read_to_string(config_path(&codex_dir)).expect("read proxy config");
    let proxy_config = proxy_config
        .parse::<DocumentMut>()
        .expect("parse proxy config");
    assert!(proxy_config["model_providers"]["custom"]
        .get("experimental_bearer_token")
        .is_none());

    switch_official_provider_with_pre_persist(Some(codex_dir.display().to_string()), |_| Ok(()))
        .expect("restore fresh official login after proxy");
    let retained: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read restored fresh login"),
    )
    .expect("parse restored fresh login");
    assert_eq!(retained, fresh_oauth);

    switch_to_proxy().expect("switch back to proxy after restoring fresh login");
    let final_proxy_auth: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read final proxy auth"),
    )
    .expect("parse final proxy auth");
    assert_eq!(final_proxy_auth, json!({"OPENAI_API_KEY": "sk-proxy"}));

    let snapshot_path =
        providers::official_snapshot_path_for_test(&codex_dir).expect("resolve snapshot path");
    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn provider_requiring_auth_without_a_key_does_not_destroy_official_login() {
    let codex_dir = temp_codex_dir("provider-missing-required-key");
    let official_config = "model_provider = \"openai\"\nmodel = \"official-model\"\n";
    let official_auth = json!({
        "auth_mode": "chatgpt",
        "tokens": {"access_token": "official-access"}
    });
    write_text(&config_path(&codex_dir), official_config).expect("write official config");
    write_json(&auth_path(&codex_dir), &official_auth).expect("write official auth");

    let error = switch_provider_with_pre_persist(
        ProviderInput {
            config_dir: Some(codex_dir.display().to_string()),
            provider_id: Some("missing-key".to_string()),
            provider_name: "Missing Key".to_string(),
            base_url: "https://proxy.example.com/v1".to_string(),
            model: "proxy-model".to_string(),
            api_key: None,
            wire_api: Some("responses".to_string()),
            requires_openai_auth: Some(true),
        },
        |_| Ok(()),
    )
    .expect_err("missing required provider key must reject the switch");

    assert!(error.to_string().contains("需要 API Key"));
    assert_eq!(
        fs::read_to_string(config_path(&codex_dir)).expect("read unchanged official config"),
        official_config
    );
    let retained_auth: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read retained official auth"),
    )
    .expect("parse retained official auth");
    assert_eq!(retained_auth, official_auth);

    let snapshot_path =
        providers::official_snapshot_path_for_test(&codex_dir).expect("resolve snapshot path");
    assert!(!snapshot_path.exists());
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn new_live_official_oauth_overrides_an_older_ready_snapshot() {
    let codex_dir = temp_codex_dir("new-live-oauth-overrides-ready-snapshot");
    write_text(
        &config_path(&codex_dir),
        "model_provider = \"openai\"\nmodel = \"official-model\"\n",
    )
    .expect("write official config");
    let previous_oauth = json!({
        "auth_mode": "chatgpt",
        "tokens": {"access_token": "previous-official-access"}
    });
    write_json(&auth_path(&codex_dir), &previous_oauth).expect("write previous official login");
    assert!(providers::capture_live_chatgpt_config(&codex_dir)
        .expect("capture previous official snapshot"));

    let fresh_oauth = json!({
        "auth_mode": "chatgpt",
        "tokens": {"access_token": "fresh-official-access"}
    });
    write_json(&auth_path(&codex_dir), &fresh_oauth).expect("write fresh official login");
    let draft = get_official_config_draft_inner(Some(codex_dir.display().to_string()))
        .expect("read official draft")
        .expect("official draft");
    let draft = serde_json::to_value(draft).expect("serialize official draft");
    let draft_auth: Value = serde_json::from_str(
        draft["authJson"]
            .as_str()
            .expect("official draft auth JSON"),
    )
    .expect("parse official draft auth JSON");
    assert_eq!(draft_auth, fresh_oauth);

    let snapshot_path =
        providers::official_snapshot_path_for_test(&codex_dir).expect("resolve snapshot path");
    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn invalid_official_auth_does_not_partially_change_live_config() {
    let codex_dir = temp_codex_dir("invalid-official-auth");
    let original_config = "model_provider = \"openai\"\nmodel = \"original-model\"\n";
    write_text(&config_path(&codex_dir), original_config).expect("write official config");

    let error = save_official_config_inner(
        Some(codex_dir.display().to_string()),
        Some("changed-model".to_string()),
        Some(
            json!({
                "auth_mode": "chatgpt",
                "tokens": {"access_token": "", "refresh_token": ""}
            })
            .to_string(),
        ),
        None,
    )
    .expect_err("empty auth must be rejected");

    assert!(error.to_string().contains("有效认证信息"));
    assert_eq!(
        fs::read_to_string(config_path(&codex_dir)).expect("read unchanged config"),
        original_config
    );
    assert!(!auth_path(&codex_dir).exists());

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
fn save_provider_toml_config_replaces_live_auth_and_restores_official_snapshot() {
    let codex_dir = temp_codex_dir("save-provider-toml-token");
    let official_auth = json!({
        "auth_mode": "chatgpt",
        "tokens": {"access_token": "official-access-token"}
    });
    write_json(&auth_path(&codex_dir), &official_auth).expect("write official auth");
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
    assert_eq!(result.message, "已切换到 Proxy");
    let config_text = fs::read_to_string(config_path(&codex_dir)).expect("read config");
    assert!(config_text.contains("model_provider = \"custom\""));
    assert!(config_text.contains("[model_providers.custom]"));
    assert!(!config_text.contains("[model_providers.proxy]"));
    let config_doc = config_text
        .parse::<DocumentMut>()
        .expect("parse provider config");
    assert!(config_doc["model_providers"]["custom"]
        .get("experimental_bearer_token")
        .is_none());
    let auth_after: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read auth after save"),
    )
    .expect("parse auth after save");
    assert_eq!(auth_after, json!({"OPENAI_API_KEY": "sk-provider-table"}));

    switch_official_provider_inner(Some(codex_dir.display().to_string()))
        .expect("restore official auth after TOML provider switch");
    let restored_auth: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read restored official auth"),
    )
    .expect("parse restored official auth");
    assert_eq!(restored_auth, official_auth);

    let snapshot_path = providers::official_snapshot_path_for_test(&codex_dir)
        .expect("resolve official snapshot path");
    let _ = fs::remove_file(snapshot_path);
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn save_provider_toml_rejects_placeholder_template_without_writing() {
    let codex_dir = temp_codex_dir("reject-placeholder-provider-toml");
    let original = "model_provider = \"openai\"\nmodel = \"official-model\"\n";
    write_text(&config_path(&codex_dir), original).expect("write original config");

    let error = save_provider_toml_config_inner(ProviderTomlInput {
        config_dir: Some(codex_dir.display().to_string()),
        config_text: r#"model_provider = "custom"
model = "gpt-5.5"

[model_providers.custom]
name = "your-provider"
base_url = "https://example.com/v1"
wire_api = "responses"
requires_openai_auth = false
"#
        .to_string(),
        api_key: None,
    })
    .expect_err("placeholder TOML must be rejected");

    assert!(error.to_string().contains("示例占位值"));
    assert_eq!(
        fs::read_to_string(config_path(&codex_dir)).expect("read unchanged config"),
        original
    );
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn provider_toml_requiring_auth_without_a_key_keeps_official_login() {
    let codex_dir = temp_codex_dir("provider-toml-missing-required-key");
    let official_config = "model_provider = \"openai\"\nmodel = \"official-model\"\n";
    let official_auth = json!({
        "auth_mode": "chatgpt",
        "tokens": {"access_token": "official-access"}
    });
    write_text(&config_path(&codex_dir), official_config).expect("write official config");
    write_json(&auth_path(&codex_dir), &official_auth).expect("write official auth");

    let error = save_provider_toml_config_inner(ProviderTomlInput {
        config_dir: Some(codex_dir.display().to_string()),
        config_text: r#"model_provider = "custom"
model = "proxy-model"

[model_providers.custom]
name = "Missing Key"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#
        .to_string(),
        api_key: None,
    })
    .expect_err("missing required provider key must reject TOML activation");

    assert!(error.to_string().contains("需要 API Key"));
    assert_eq!(
        fs::read_to_string(config_path(&codex_dir)).expect("read unchanged official config"),
        official_config
    );
    let retained_auth: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read retained official auth"),
    )
    .expect("parse retained official auth");
    assert_eq!(retained_auth, official_auth);

    let snapshot_path =
        providers::official_snapshot_path_for_test(&codex_dir).expect("resolve snapshot path");
    assert!(!snapshot_path.exists());
    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn provider_toml_activation_uses_complete_provider_template() {
    let codex_dir = temp_codex_dir("provider-toml-merge");
    let current = r#"# keep-user-comment
model_provider = "custom"
model = "old-model"
approval_policy = "never"
model_instructions_file = "/tmp/user-prompt.md"

[model_providers.custom]
name = "Old provider"
base_url = "https://old.example.com/v1"
wire_api = "responses"
requires_openai_auth = false

[mcp_servers.docs]
command = "docs-server"

[projects."/tmp/work"]
trust_level = "trusted"
"#;
    write_text(&config_path(&codex_dir), current).expect("write current config");

    save_provider_toml_config_inner(ProviderTomlInput {
        config_dir: Some(codex_dir.display().to_string()),
        config_text: r#"model_provider = "proxy"
model = "new-model"
approval_policy = "on-request"

[model_providers.proxy]
name = "New provider"
base_url = "https://new.example.com/v1"
wire_api = "responses"
requires_openai_auth = false

[mcp_servers.stale]
command = "must-not-replace-live"
"#
        .to_string(),
        api_key: Some("sk-new".to_string()),
    })
    .expect("merge provider config");

    let merged = fs::read_to_string(config_path(&codex_dir)).expect("read merged config");
    let doc = merged
        .parse::<DocumentMut>()
        .expect("merged config stays valid");
    assert!(!merged.contains("# keep-user-comment"));
    assert_eq!(doc["model"].as_str(), Some("new-model"));
    assert_eq!(doc["approval_policy"].as_str(), Some("on-request"));
    assert!(doc.get("model_instructions_file").is_none());
    assert_eq!(
        doc["mcp_servers"]["stale"]["command"].as_str(),
        Some("must-not-replace-live")
    );
    assert!(doc["mcp_servers"].get("docs").is_none());
    assert!(doc.get("projects").is_none());
    assert_eq!(
        doc["model_providers"]["custom"]["base_url"].as_str(),
        Some("https://new.example.com/v1")
    );
    assert!(doc["model_providers"]["custom"]
        .get("experimental_bearer_token")
        .is_none());
    let auth: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read merged provider auth"),
    )
    .expect("parse merged provider auth");
    assert_eq!(auth, json!({"OPENAI_API_KEY": "sk-new"}));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn save_provider_toml_migrates_existing_scoped_token_to_auth() {
    let codex_dir = temp_codex_dir("save-provider-toml-existing-token");
    let result = save_provider_toml_config_inner(ProviderTomlInput {
        config_dir: Some(codex_dir.display().to_string()),
        config_text: r#"model_provider = "custom"
model = "gpt-5.5"

[model_providers.custom]
name = "Proxy"
base_url = "https://proxy.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
experimental_bearer_token = "sk-existing"
"#
        .to_string(),
        api_key: None,
    })
    .expect("save provider toml using existing token");

    assert!(result.ok);
    let config = fs::read_to_string(config_path(&codex_dir)).expect("read live config");
    let config = config
        .parse::<DocumentMut>()
        .expect("parse live provider config");
    assert!(config["model_providers"]["custom"]
        .get("experimental_bearer_token")
        .is_none());
    let auth: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read migrated provider auth"),
    )
    .expect("parse migrated provider auth");
    assert_eq!(auth, json!({"OPENAI_API_KEY": "sk-existing"}));

    let _ = fs::remove_dir_all(codex_dir);
}

#[test]
fn save_provider_toml_pre_switch_hook_observes_current_provider() {
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
    let live_doc = result
        .state
        .config_text
        .parse::<DocumentMut>()
        .expect("parse provider B live config");
    assert!(live_doc["model_providers"]["custom"]
        .get("experimental_bearer_token")
        .is_none());
    assert!(!result
        .state
        .config_text
        .contains("https://a.example.com/v1"));
    let live_auth: Value = serde_json::from_str(
        &fs::read_to_string(auth_path(&codex_dir)).expect("read provider B auth"),
    )
    .expect("parse provider B auth");
    assert_eq!(live_auth, json!({"OPENAI_API_KEY": "sk-b"}));

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
    write_text(
        &codex_dir.join("config.toml"),
        "model_provider = \"custom\"\n",
    )
    .expect("write shared provider config");
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
    write_text(
        &codex_dir.join("config.toml"),
        "model_provider = \"custom\"\n",
    )
    .expect("write shared provider config");
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
fn provider_sync_updates_all_provider_indexes_without_touching_cwd_or_user_flag() {
    let codex_dir = temp_codex_dir("target-provider-all-dbs");
    write_text(
        &codex_dir.join("config.toml"),
        "model_provider = \"custom\"\n",
    )
    .expect("write shared provider config");
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
    .expect("sync all session databases");
    assert_eq!(result.updated_rollouts, 1);
    assert_eq!(result.updated_threads, 2);
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
            ("custom".to_string(), 0, "/tmp/wrong".to_string())
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
    write_text(
        &codex_dir.join("config.toml"),
        "model_provider = \"custom\"\n",
    )
    .expect("write shared provider config");
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
