//! Provider 适配与配置（B4a）：配置模型、密钥托管、能力协商、连接测试、
//! 主动回应入口与错误归一化。
//!
//! 消费契约见 `docs/architecture/m3-Provider-配置与能力.md`，这里只把口径写成代码。
//!
//! # 三条不可动摇的口径
//!
//! 1. **默认不联网**：出网只发生在两个地方——用户显式发起的 `providers.test`，
//!    与真被领取执行的 Provider 作业（`respond`）。没有「启动探测」「后台预热」
//!    「悄悄试一下」这类路径。没有已配置且启用的服务时，核心连一个请求都不发；
//! 2. **能力不成立就不排任务**：能不能用某个能力，由「配置声明 + 适配器实现 +
//!    连接测试验证」三者共同决定（见 `capability_gate_for`）。不满足时排任务入口
//!    直接拒绝，理由是给用户看的、下一步是可执行的；
//! 3. **密钥只以引用出现**：`providers.credential_ref` 是引用，明文只在
//!    `SecretStore` 里、只在拼请求头的那一刻取出。日志、错误、返回给上层的对象
//!    都不含明文。

pub mod adapter;
pub mod local_engine;
pub mod normalize;
pub mod secret;
pub mod transport;

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

use crate::error::{CoreError, Result};
use crate::model::{
    AiResponse, Job, JobPriority, NewJob, Provider, ProviderCapability, ProviderTestReport,
    ProviderTestStatus, ResponseTicket,
};
use crate::support::now;
use crate::{support, Core};

/// 「用户主动请求一次回应」的作业 kind。
///
/// 契约 `responses.request` 只返回 jobId：真正的调用由队列在驱动里跑，
/// 所以「只有主动调用才运行」在数据上就成立了——没有请求行就没有任务。
pub(crate) const KIND_RESPOND: &str = "respond";

/// 作业终态失败码：目标（回应请求）不存在。重试不会变好。
pub(crate) const ERROR_NO_TARGET: &str = "no_target";

/// `respond` 作业的优先级：用户自己点的，排在后台整理之前。
const RESPOND_PRIORITY: JobPriority = JobPriority::UserOrganize;

// ------------------------------------------------------------------ 读

const PROVIDER_COLUMNS: &str = "id, label, adapter_id, endpoint, model, declared_capabilities, \
                                verified_capabilities, credential_ref, enabled, test_status";

/// 更新配置时先读出来的旧行：功能参数（协议、端点、模型、凭据引用）、创建时间，
/// 以及**验证结论**（已验能力、测试状态、失败码与测试时间）。
///
/// 验证结论要一起读出来是有原因的（#96）：只有功能参数真的变了，这一组才作废；
/// 只改显示名或 `enabled` 开关时得把旧结论原样写回去。
struct ExistingProvider {
    adapter_id: String,
    endpoint: Option<String>,
    model: Option<String>,
    credential_ref: Option<String>,
    created_at: String,
    verified_capabilities: String,
    test_status: String,
    last_error_code: Option<String>,
    last_tested_at: Option<String>,
}

type RawProvider = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
    i64,
    String,
);

fn provider_from_raw(raw: RawProvider) -> Result<Provider> {
    let (
        id,
        label,
        adapter_id,
        endpoint,
        model,
        declared,
        _verified,
        credential_ref,
        enabled,
        test_status,
    ) = raw;
    Ok(Provider {
        id,
        label,
        adapter_id,
        endpoint,
        model,
        capabilities: parse_capabilities(&declared)?,
        credential_ref,
        enabled: enabled != 0,
        test_status: ProviderTestStatus::from_wire(&test_status).ok_or_else(|| {
            CoreError::CorruptedData {
                message: format!("未知的连接测试状态：{test_status}"),
            }
        })?,
    })
}

/// 能力集合的 JSON 表示。未知名字**直接忽略**（前向兼容：新版本加的能力不该让
/// 旧版本连配置都读不出来），但不会因此被当成「支持」——闸门只认知道的那些。
fn parse_capabilities(json: &str) -> Result<HashSet<ProviderCapability>> {
    let names: Vec<String> = serde_json::from_str(json).map_err(|error| CoreError::CorruptedData {
        message: format!("能力集合不是合法的 JSON 数组：{error}"),
    })?;
    Ok(names
        .iter()
        .filter_map(|name| ProviderCapability::from_wire(name))
        .collect())
}

fn capabilities_json(capabilities: &HashSet<ProviderCapability>) -> Result<String> {
    let mut names: Vec<&str> = capabilities.iter().map(|item| item.wire()).collect();
    names.sort_unstable();
    Ok(serde_json::to_string(&names)?)
}

pub(crate) fn load_provider(conn: &rusqlite::Connection, provider_id: &str) -> Result<Provider> {
    let sql = format!("SELECT {PROVIDER_COLUMNS} FROM providers WHERE id = ?1");
    let raw: Option<RawProvider> = conn
        .query_row(&sql, params![provider_id], raw_provider_row)
        .optional()?;
    match raw {
        Some(row) => provider_from_raw(row),
        None => Err(CoreError::NotFound {
            entity: "服务配置",
            id: provider_id.to_owned(),
        }),
    }
}

fn raw_provider_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawProvider> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

/// 某个服务最近一次连接测试验过的能力。**不在 Provider 对象里**（契约没有这个
/// 字段），只参与闸门判断。
fn verified_capabilities(
    conn: &rusqlite::Connection,
    provider_id: &str,
) -> Result<HashSet<ProviderCapability>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT verified_capabilities FROM providers WHERE id = ?1",
            params![provider_id],
            |row| row.get(0),
        )
        .optional()?;
    match json {
        Some(json) => parse_capabilities(&json),
        None => Ok(HashSet::new()),
    }
}

pub(crate) fn provider_secret(core: &Core, provider: &Provider) -> Result<Option<String>> {
    match provider.credential_ref.as_deref() {
        Some(credential_ref) => core.secrets().get(credential_ref),
        None => Ok(None),
    }
}

// ------------------------------------------------------------------ 列表 / 保存 / 移除

/// 表里**存下来的**服务配置。`providers.list` 返回的就是它。
///
/// 核心自带的本机转写引擎**不是**这里的一行（见 `local_engine` 的模块文档：
/// 内建行不进库，否则用户删掉之后下次开库又会原样长回来）。它只出现在
/// 「转写这一路能不能用」的判据里（`transcription_choice`）。
pub(crate) fn stored_providers(core: &Core) -> Result<Vec<Provider>> {
    let sql = format!("SELECT {PROVIDER_COLUMNS} FROM providers ORDER BY created_at, id");
    let mut statement = core.conn.prepare(&sql)?;
    let rows = statement
        .query_map([], raw_provider_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(provider_from_raw).collect()
}

pub(crate) fn list(core: &Core) -> Result<Vec<Provider>> {
    stored_providers(core)
}

/// 校验并规范化 endpoint：只接受 `http(s)://host[:port][/path]`。
///
/// 不用通用 URL 解析库：这里要的只是「协议是 http/https、有非空主机、没有空白」，
/// 多引一个依赖不划算。**不校验主机能不能解析**——那是连接测试的事。
fn normalize_endpoint(endpoint: &str) -> Result<String> {
    let trimmed = endpoint.trim();
    let (scheme, rest) = trimmed.split_once("://").ok_or_else(|| {
        CoreError::InvalidArgument {
            reason: format!("endpoint「{endpoint}」缺少协议前缀（要写 http:// 或 https://）"),
        }
    })?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(CoreError::InvalidArgument {
            reason: format!("endpoint 的协议只支持 http / https，实际是「{scheme}」"),
        });
    }
    if rest.chars().any(char::is_whitespace) {
        return Err(CoreError::InvalidArgument {
            reason: format!("endpoint「{endpoint}」里不能有空白字符"),
        });
    }
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim();
    if host.is_empty() {
        return Err(CoreError::InvalidArgument {
            reason: format!("endpoint「{endpoint}」没有主机名"),
        });
    }
    Ok(format!("{scheme}://{}", rest.trim_end_matches('/')))
}

/// endpoint 属于本机/局域网还是远端。
///
/// 用途：连接测试报告里**可识别地**说清「你在测一个本机服务还是远端服务」，
/// 免得用户以为资料发去了远端（契约与任务书都要求这两者可识别地区分）。
/// 判定只看主机名，不解析 DNS（`example.local` 算局域网，域名算远端）。
pub fn endpoint_scope(endpoint: &str) -> &'static str {
    let host = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    // 去掉 userinfo 与端口。
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        host.split(':').next().unwrap_or(host)
    };
    let host = host.to_ascii_lowercase();

    let is_private_v4 = || {
        let octets: Vec<&str> = host.split('.').collect();
        if octets.len() != 4 {
            return false;
        }
        let first: u8 = octets[0].parse().unwrap_or(255);
        let second: u8 = octets[1].parse().unwrap_or(255);
        first == 10
            || (first == 172 && (16..=31).contains(&second))
            || (first == 192 && second == 168)
            || (first == 169 && second == 254)
            || first == 127
    };

    if host == "localhost"
        || host == "::1"
        || host == "0.0.0.0"
        || host.ends_with(".local")
        || is_private_v4()
        // 没有点的名字基本都是局域网主机名（`nas`、`ollama`…）。
        || !host.contains('.')
    {
        "本机/局域网"
    } else {
        "远端"
    }
}

pub(crate) fn save(core: &mut Core, config: &Provider, secret: Option<&str>) -> Result<Provider> {
    let label = config.label.trim();
    if label.is_empty() {
        return Err(CoreError::InvalidArgument {
            reason: "服务配置需要一个显示名（label）".to_owned(),
        });
    }
    let adapter = adapter::adapter_for(&config.adapter_id).ok_or_else(|| {
        CoreError::InvalidArgument {
            reason: format!(
                "没有标识为「{}」的协议适配器；内置的有：{}",
                config.adapter_id,
                adapter::builtin_adapter_ids().join("、")
            ),
        }
    })?;
    // 本地引擎没有 endpoint 也没有密钥：给它配一个 http:// 地址是配置错误，
    // 如实拒绝（而不是存下来一个永远不会被用的字段，让用户以为配上了什么）。
    if adapter.is_local() {
        if config
            .endpoint
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
        {
            return Err(CoreError::InvalidArgument {
                reason: format!(
                    "适配器「{}」是本机引擎，不使用 endpoint（endpoint 是 HTTP 协议的事）；                     要换模型目录请填 model=<目录>，删掉 endpoint 再保存",
                    config.adapter_id
                ),
            });
        }
        // `model` 留空是**合法**的：表示「用随包/环境变量解析出来的模型目录」。
        // 本地引擎的 model 是一份**目录**（不是远端那种模型名），这一点写在
        // `docs/architecture/m3-转写链.md` 的「模型目录」一节。
        let model = config
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let provider_id = if config.id.trim().is_empty() {
            support::new_id("prv")
        } else {
            config.id.trim().to_owned()
        };
        return save_local(core, config, &adapter, provider_id, label, model);
    }
    // 适配器在下面只用来校验存在性；这里显式消费掉，避免「绑了不用」。
    let _adapter_id = adapter.id();

    let endpoint = match config.endpoint.as_deref() {
        Some(value) if !value.trim().is_empty() => Some(normalize_endpoint(value)?),
        _ => None,
    };
    let model = config
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    let provider_id = if config.id.trim().is_empty() {
        support::new_id("prv")
    } else {
        config.id.trim().to_owned()
    };

    // 更新时先看旧行：`created_at` 要保留，而且**功能参数（适配器/端点/模型/密钥）
    // 变了，旧的验证结论就作废**（那是对旧配置的验证）。只改显示名或开关不动结论。
    let existing: Option<ExistingProvider> = core
        .conn
            .query_row(
                "SELECT adapter_id, endpoint, model, credential_ref, created_at, \
                 verified_capabilities, test_status, last_error_code, last_tested_at \
                 FROM providers WHERE id = ?1",
                params![provider_id],
                |row| {
                    Ok(ExistingProvider {
                        adapter_id: row.get(0)?,
                        endpoint: row.get(1)?,
                        model: row.get(2)?,
                        credential_ref: row.get(3)?,
                        created_at: row.get(4)?,
                        verified_capabilities: row.get(5)?,
                        test_status: row.get(6)?,
                        last_error_code: row.get(7)?,
                        last_tested_at: row.get(8)?,
                    })
                },
            )
            .optional()?;

    // 密钥：给了新明文就存新的（旧引用没人再用就删掉）；没给就沿用配置里的引用；
    // 引用为空表示「清掉凭据」。
    let old_ref = existing
        .as_ref()
        .and_then(|row| row.credential_ref.clone());
    let new_ref = match secret.map(str::trim) {
        Some(value) if !value.is_empty() => Some(core.secrets().put(&provider_id, value)?),
        Some(_) => {
            return Err(CoreError::InvalidArgument {
                reason: "密钥不能是空白；要清掉凭据就把 credentialRef 留空".to_owned(),
            })
        }
        None => config.credential_ref.clone(),
    };
    // 旧引用的清理放到 UPSERT **之后**（见下）：先删文件的话，表里那一行还指着它，
    // `forget_secret` 会以为「还有人用」而不删。
    let stale_ref = old_ref.filter(|old| Some(old) != new_ref.as_ref());

    // 「功能参数变化」= 适配器 / 端点 / 模型 / 密钥。**显示名与 `enabled` 开关不算**：
    // 它们不改变远端收到的请求，凭什么作废一次真实的验证结论——作废的后果很具体，
    // 失败结论被洗成 `untested` 后，能力闸门会跳过「验过没有」这一条，把服务重新放行
    // （#96；评审给的两个触发点：改标签、切开关）。
    let functional_change = match &existing {
        None => true,
        Some(old) => {
            old.adapter_id != config.adapter_id
                || old.endpoint.as_deref() != endpoint.as_deref()
                || old.model.as_deref() != model.as_deref()
                || secret_changed(
                    core,
                    old.credential_ref.as_deref(),
                    new_ref.as_deref(),
                    secret,
                )?
        }
    };

    let declared = capabilities_json(&config.capabilities)?;
    // 验证结论三件套（`test_status` / `verified_capabilities` / `last_error_code`）：
    // 功能参数没变就**原样保留**，变了才一起清回「没测过」（`last_tested_at` 同理——
    // 留着上一次的时间会让人以为新配置测过）。
    let (verified, test_status, last_error_code, last_tested_at) =
        match (&existing, functional_change) {
            (Some(old), false) => (
                old.verified_capabilities.clone(),
                old.test_status.clone(),
                old.last_error_code.clone(),
                old.last_tested_at.clone(),
            ),
            _ => (
                "[]".to_owned(),
                ProviderTestStatus::Untested.wire().to_owned(),
                None,
                None,
            ),
        };
    let created_at = match &existing {
        Some(old) => old.created_at.clone(),
        None => support::to_iso(now()),
    };
    let now_text = support::to_iso(now());

    core.conn.execute(
        "INSERT INTO providers (id, label, adapter_id, endpoint, model, declared_capabilities, \
         verified_capabilities, credential_ref, enabled, test_status, last_tested_at, \
         last_error_code, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
         ON CONFLICT(id) DO UPDATE SET label = excluded.label, adapter_id = excluded.adapter_id, \
         endpoint = excluded.endpoint, model = excluded.model, \
         declared_capabilities = excluded.declared_capabilities, \
         verified_capabilities = excluded.verified_capabilities, \
         credential_ref = excluded.credential_ref, enabled = excluded.enabled, \
         test_status = excluded.test_status, last_tested_at = excluded.last_tested_at, \
         last_error_code = excluded.last_error_code, updated_at = excluded.updated_at",
        params![
            provider_id,
            label,
            config.adapter_id,
            endpoint,
            model,
            declared,
            verified,
            new_ref,
            i64::from(config.enabled),
            test_status,
            last_tested_at,
            last_error_code,
            created_at,
            now_text,
        ],
    )?;

    if let Some(stale_ref) = stale_ref {
        forget_secret(core, &stale_ref)?;
    }

    // 配置一变，之前「等配置」的任务可能就能跑了（也可能反过来）。两边都重评一次。
    promote_configuration_waits(core, now())?;
    load_provider(&core.conn, &provider_id)
}

/// 保存一条**本机引擎**的服务配置。
///
/// 与 HTTP 那条路的差别就三处，其余（幂等、验证结论作废、等配置重评）完全一致：
///
/// 1. **没有 endpoint、没有密钥**：本地引擎不出网，也就没有凭据可托管。
///    `credential_ref` 一律存 NULL（给它配密钥没有意义，收下来只会让人以为
///    「这个服务是会出网的」）；
/// 2. **`model` 是一份目录**，留空表示「用随包/环境变量解析出来的默认目录」。
///    这里**不校验目录存在**：目录可能还没随包放进来，那属于「等配置」，
///    由能力闸门如实报出来（保存配置本身不该失败）；
/// 3. **`verified_capabilities` 的作废判据里包含 `model`**：换了模型目录，
///    上一次的连接测试结论就不再成立（那是对旧目录的验证）。
fn save_local(
    core: &mut Core,
    config: &Provider,
    adapter: &adapter::Adapter,
    provider_id: String,
    label: &str,
    model: Option<String>,
) -> Result<Provider> {
    let existing: Option<(String, Option<String>, String, String)> = core
        .conn
        .query_row(
            "SELECT adapter_id, model, created_at, verified_capabilities FROM providers \
             WHERE id = ?1",
            params![provider_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let config_changed = match &existing {
        None => true,
        Some((adapter_id, old_model, _, _)) => {
            adapter_id != &config.adapter_id || old_model.as_deref() != model.as_deref()
        }
    };
    let verified = match &existing {
        Some((_, _, _, verified)) if !config_changed => verified.clone(),
        _ => "[]".to_owned(),
    };
    let created_at = match &existing {
        Some((_, _, created_at, _)) => created_at.clone(),
        None => support::to_iso(now()),
    };
    let declared = capabilities_json(&config.capabilities)?;
    let now_text = support::to_iso(now());

    core.conn.execute(
        "INSERT INTO providers (id, label, adapter_id, endpoint, model, declared_capabilities, \
         verified_capabilities, credential_ref, enabled, test_status, last_tested_at, \
         last_error_code, created_at, updated_at) \
         VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, NULL, ?7, ?8, NULL, NULL, ?9, ?10) \
         ON CONFLICT(id) DO UPDATE SET label = excluded.label, adapter_id = excluded.adapter_id, \
         endpoint = NULL, model = excluded.model, \
         declared_capabilities = excluded.declared_capabilities, \
         verified_capabilities = excluded.verified_capabilities, credential_ref = NULL, \
         enabled = excluded.enabled, test_status = excluded.test_status, \
         updated_at = excluded.updated_at",
        params![
            provider_id,
            label,
            config.adapter_id,
            model,
            declared,
            verified,
            i64::from(config.enabled),
            ProviderTestStatus::Untested.wire(),
            created_at,
            now_text,
        ],
    )?;
    // 适配器在这里只用来校验过存在性与形态；显式消费掉，避免「绑了不用」。
    let _adapter_id = adapter.id();

    // 配置一变，等在「配置」上的任务可能就能跑了。
    promote_configuration_waits(core, now())?;
    load_provider(&core.conn, &provider_id)
}

/// 密钥算不算「功能参数变化」。**按明文比，不按引用比**：`SecretStore::put` 每次
/// 都发一个新引用，拿引用比会把「原样重填同一把钥匙」也当成变化——而设置页每次保存
/// 都会把当前密钥一起提交，那等于每次保存都作废验证结论，正是 #96 要修掉的那类误判。
///
/// 给不出旧明文（以前没凭据、或凭据已经不在托管里）就算变化：那确实可能换了一把钥匙。
/// 没给新明文时只看引用变没变（换引用、或清空凭据）。
fn secret_changed(
    core: &Core,
    old_ref: Option<&str>,
    new_ref: Option<&str>,
    secret: Option<&str>,
) -> Result<bool> {
    match secret.map(str::trim) {
        Some(value) if !value.is_empty() => match old_ref {
            Some(old_ref) => Ok(core.secrets().get(old_ref)?.as_deref() != Some(value)),
            None => Ok(true),
        },
        _ => Ok(old_ref != new_ref),
    }
}

/// 移除配置。**不删除任何日记、也不删除已经生成的回应**：回应只是历史事实，
/// 它记着当时的 provider/model，配置没了不代表那段历史不存在。
pub(crate) fn remove(core: &mut Core, provider_id: &str) -> Result<()> {
    if provider_id == local_engine::BUILTIN_LOCAL_ASR_ID {
        // 内建引擎不是一条配置行：它随包提供。删「不存在的东西」不能装作成功，
        // 也不能报一个让人以为「已经被删掉了、转写没了」的错——如实说清两条路。
        if load_provider(&core.conn, provider_id).is_err() {
            return Err(CoreError::InvalidState {
                entity: "服务配置",
                id: provider_id.to_owned(),
                state: format!(
                    "{}是核心自带的本机引擎，没有可移除的配置行；                     要停用就保存一条同 id、enabled=false 的配置，要换模型目录就保存 model=<目录>",
                    local_engine::BUILTIN_LOCAL_ASR_LABEL
                ),
            });
        }
    }
    let provider = load_provider(&core.conn, provider_id)?;
    core.conn
        .execute("DELETE FROM providers WHERE id = ?1", params![provider_id])?;
    if let Some(credential_ref) = provider.credential_ref {
        forget_secret(core, &credential_ref)?;
    }
    promote_configuration_waits(core, now())?;
    Ok(())
}

/// 删掉一个凭据引用——**只有没有别的服务还在用它时**才真删。
///
/// 多行共享同一个引用是允许的（例如同一个人两个模型用同一把密钥），所以不能
/// 见到引用就删。
fn forget_secret(core: &Core, credential_ref: &str) -> Result<()> {
    let still_used: i64 = core.conn.query_row(
        "SELECT COUNT(*) FROM providers WHERE credential_ref = ?1",
        params![credential_ref],
        |row| row.get(0),
    )?;
    if still_used == 0 {
        core.secrets().delete(credential_ref)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ 能力闸门

/// 能力闸门的结论：能不能用、为什么、下一步做什么。
///
/// `reason` 与 `next_step` 都是**给用户看的**：不满足时必须可执行（去配置什么、
/// 去点哪一步），不能只说「不可用」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityGate {
    pub ready: bool,
    /// 可用时是选中的服务；不可用时为空。
    pub provider_id: Option<String>,
    /// 不能排任务时，作业该记哪个错误码（契约第 7 节的码）。
    ///
    /// 正常情况下是 `provider_not_configured`；如果这个服务最近一次就是**鉴权失败**，
    /// 那么如实报 `authentication_failed`——用户看到的原因不同，动作也不同
    /// （一个去补配置，一个去改密钥），但在队列侧都是「等配置、不频繁重试」。
    pub code: String,
    pub reason: String,
    pub next_step: Option<String>,
}

impl CapabilityGate {
    fn ready(provider_id: &str, reason: String) -> Self {
        Self {
            ready: true,
            provider_id: Some(provider_id.to_owned()),
            code: String::new(),
            reason,
            next_step: None,
        }
    }

    fn blocked(reason: String, next_step: String, code: &str) -> Self {
        Self {
            ready: false,
            provider_id: None,
            code: code.to_owned(),
            reason,
            next_step: Some(next_step),
        }
    }
}

/// 一个服务能不能承担某个能力。**这些条件缺一不可**：
///
/// 1. 已启用；
/// 2. 适配器真的实现了这个能力；
/// 3. 配置里声明了这个能力；
/// 4. 没有测试结论，或者最近一次测试验过这个能力；
/// 5. 端点、模型、密钥都在（少了任何一样，执行时必然失败——那就不该排任务）。
///
/// 第 4 条是关键口径：**测过但没验过的能力不再排任务**。用户明确「声明」过、
/// 还没测过的能力仍然可用（那是「明确配置」这一路），但一旦连接测试说了不行，
/// 就不能装作行——否则就是排一条注定失败的任务，与 #67/#74 的口径冲突。
///
/// 第 5 条把「配置不完整」挡在**排任务之前**：没有密钥的服务不该先排一条任务、
/// 等领取执行时才发现没密钥（那时候已经消耗了一次尝试）。
fn evaluate(
    core: &Core,
    provider: &Provider,
    capability: ProviderCapability,
) -> Result<std::result::Result<(), String>> {
    let conn = &core.conn;
    if !provider.enabled {
        return Ok(Err(format!("服务「{}」没有启用", provider.label)));
    }
    let Some(adapter) = adapter::adapter_for(&provider.adapter_id) else {
        return Ok(Err(format!(
            "服务「{}」用的适配器「{}」不在本构建里",
            provider.label, provider.adapter_id
        )));
    };
    if !adapter.capabilities().contains(&capability) {
        return Ok(Err(format!(
            "适配器「{}」没有实现 {}（内置适配器实现了：{}）",
            provider.adapter_id,
            capability.wire(),
            adapter
                .capabilities()
                .iter()
                .map(|item| item.wire())
                .collect::<Vec<_>>()
                .join("、")
        )));
    }
    if !provider.capabilities.contains(&capability) {
        return Ok(Err(format!(
            "服务「{}」的配置里没有声明 {}",
            provider.label,
            capability.wire()
        )));
    }
    if provider.test_status != ProviderTestStatus::Untested {
        let verified = verified_capabilities(conn, &provider.id)?;
        if !verified.contains(&capability) {
            return Ok(Err(format!(
                "服务「{}」最近一次连接测试没有验证通过 {}",
                provider.label,
                capability.wire()
            )));
        }
    }

    // 本地引擎：没有 endpoint、没有密钥，改为**本机资源就绪**这一条。
    // 「缺什么、怎么办」由引擎的就绪结论直接给出（带路径的那种）。
    if adapter.is_local() {
        return match local_engine::readiness_for(core, provider.model.as_deref()) {
            Ok(readiness) if readiness.ready => Ok(Ok(())),
            Ok(readiness) => Ok(Err(local_engine::blocked_message(&readiness))),
            Err(error) => Ok(Err(error.to_string())),
        };
    }

    if provider
        .endpoint
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        return Ok(Err(format!("服务「{}」还没有配置 endpoint", provider.label)));
    }
    if provider
        .model
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        return Ok(Err(format!("服务「{}」还没有配置模型名", provider.label)));
    }
    match provider.credential_ref.as_deref() {
        None => Ok(Err(format!("服务「{}」还没有配置密钥", provider.label))),
        Some(reference) => match core.secrets().get(reference)? {
            Some(_) => Ok(Ok(())),
            None => Ok(Err(format!(
                "服务「{}」的凭据已经不在密钥托管里；请重新填写密钥",
                provider.label
            ))),
        },
    }
}

/// 本机引擎这条路的「下一步」：与 HTTP 那条路的措辞不同，因为要做的事不同
/// （补模型目录/引擎库，而不是配 endpoint 与密钥）。
fn local_next_step(capability: ProviderCapability) -> String {
    format!(
        "本机引擎不需要 endpoint 与密钥：把模型目录（model.int8.onnx 与 tokens.txt）放到随包位置 \
         {}，或用 {} 指定目录、{} 指定引擎库；也可以在设置里保存一条 adapter={} 的服务配置，\
         把 model 填成你自己的模型目录。配好后对这条服务跑一次连接测试（对 {} 而言，\
         「连接测试」= 本机加载模型并转写 0.05 秒静音，不联网）。",
        local_engine::BUNDLED_MODEL_REL,
        local_engine::ENV_MODEL_DIR,
        local_engine::ENV_ENGINE_LIB,
        local_engine::SHERPA_ONNX,
        capability.wire()
    )
}

/// 按能力挑服务。`only` 指定时只考虑这一个（`responses.request(providerId)` 与
/// 作业里钉死的服务走这条路），不指定时按「测试通过优先，其次没测过」挑一个。
pub(crate) fn capability_gate_for(
    core: &Core,
    capability: ProviderCapability,
    only: Option<&str>,
) -> Result<CapabilityGate> {
    capability_gate_for_with(core, capability, only, false)
}

/// 与 [`capability_gate_for`] 相同，但可以要求**只考虑本机引擎**。
///
/// `local_only` 用在录音的转写链上：那条链跑的是本机引擎（`transcription` 模块），
/// 远端转写不在它的执行范围里。让闸门把远端服务也算成「可用」会让用户看到
/// 「转写可用」，然后任务跑起来用的是另一个东西——闸门必须只说执行者做得到的事。
pub(crate) fn capability_gate_for_with(
    core: &Core,
    capability: ProviderCapability,
    only: Option<&str>,
    local_only: bool,
) -> Result<CapabilityGate> {
    let next_step = |what: &str| {
        format!(
            "在「设置 → 远端服务」里{what}，然后对「{}」跑一次连接测试；\
             本机/局域网服务把 endpoint 配成 http://127.0.0.1:<端口>/v1 这类地址即可",
            capability.wire()
        )
    };

    if let Some(provider_id) = only {
        // 指定的服务可能已经被移除（作业里钉死的服务就是这样）。这不是调用方的
        // `not_found`：它只是「这条任务在等的配置不在了」，要给可执行的下一步。
        let provider = match load_provider(&core.conn, provider_id) {
            Ok(provider) => provider,
            Err(CoreError::NotFound { .. }) => {
                return Ok(CapabilityGate::blocked(
                    format!("服务「{provider_id}」的配置已经不在了"),
                    next_step("重新配置这个服务"),
                    "provider_not_configured",
                ))
            }
            Err(other) => return Err(other),
        };
        let last_error = provider_last_error(&core.conn, provider_id)?;
        let is_local = adapter::adapter_for(&provider.adapter_id)
            .map(|adapter| adapter.is_local())
            .unwrap_or(false);
        let describe = |provider: &Provider| {
            if is_local {
                format!(
                    "服务「{}」（本机引擎 {}，模型目录 {}）可承担 {}",
                    provider.label,
                    provider.adapter_id,
                    provider.model.as_deref().unwrap_or("<随包/环境变量解析>"),
                    capability.wire()
                )
            } else {
                format!(
                    "服务「{}」（{}，{}）可承担 {}",
                    provider.label,
                    provider.adapter_id,
                    provider
                        .endpoint
                        .as_deref()
                        .map(endpoint_scope)
                        .unwrap_or("未配 endpoint"),
                    capability.wire()
                )
            }
        };
        return Ok(match evaluate(core, &provider, capability)? {
            Ok(()) => CapabilityGate::ready(&provider.id, describe(&provider)),
            Err(reason) => CapabilityGate::blocked(
                reason.clone(),
                if is_local {
                    local_next_step(capability)
                } else {
                    next_step(&format!("修好「{}」：{reason}", provider.label))
                },
                blocked_code(last_error.as_deref()),
            ),
        });
    }

    // 候选 = 表里存下来的服务配置（启用中）。
    let stored = stored_providers(core)?;
    let mut providers: Vec<Provider> = stored
        .iter()
        .filter(|item| item.enabled)
        .cloned()
        .collect();
    if local_only {
        providers.retain(|provider| {
            adapter::adapter_for(&provider.adapter_id)
                .map(|adapter| adapter.is_local())
                .unwrap_or(false)
        });
    }
    // 转写这一路还有**核心自带的本机引擎**（随包模型）。它不是配置行，只在
    // 「本机引擎能不能用」这个判据里出现（见 `local_engine::transcription_choice`）。
    //
    // **只有在一条本机引擎配置行都没有时**才把它加进来：有配置行时由那一行说了算
    // ——哪怕它配错了。理由是闸门与执行者必须说同一件事：`transcription_choice`
    // 取的就是「第一条启用中的本机引擎配置行」，如果在闸门这边落到内建引擎、
    // 执行者却去读那行的目录，就会出现「闸门说可用、执行者说缺模型」的抖动。
    // 坏掉的显式配置也不会静默退回默认：用户明确指了一个目录，就该按那个目录报错。
    let consider_builtin = capability == ProviderCapability::TranscribeAudio
        && !providers.iter().any(|provider| {
            adapter::adapter_for(&provider.adapter_id)
                .map(|adapter| adapter.is_local())
                .unwrap_or(false)
        });
    if consider_builtin {
        providers.push(local_engine::builtin_provider());
    }

    if providers.is_empty() {
        let reason = if stored.is_empty() {
            if local_only {
                format!("还没有可用的本机引擎配置，不能用 {}", capability.wire())
            } else {
                format!("还没有配置任何远端服务，不能用 {}", capability.wire())
            }
        } else if local_only {
            format!(
                "有 {} 个服务配置，但没有一个能承担 {}（都没启用，或不是本机引擎）",
                stored.len(),
                capability.wire()
            )
        } else {
            // 与 B4a 的措辞保持一致：这条路（远端能力）下「没有候选」只可能是
            // 「一个都没启用」。
            format!(
                "有 {} 个服务配置，但一个都没有启用，不能用 {}",
                stored.len(),
                capability.wire()
            )
        };
        return Ok(CapabilityGate::blocked(
            reason,
            if local_only {
                local_next_step(capability)
            } else {
                next_step("添加并启用一个服务")
            },
            "provider_not_configured",
        ));
    }
    let enabled: Vec<&Provider> = providers.iter().collect();

    // 挑选顺序：测试通过的优先，其次没测过，最后是最近测过但没验过的。同一档里
    // 按创建顺序（`list` 已排序），结果确定。
    //
    // 三档都要**评估**（不只是前两档）：全都不可用时，理由要来自真实的那一个
    // （「最近一次连接测试没验过」），不能笼统说成「没有声明这个能力」。
    let mut hints: Vec<String> = Vec::new();
    let mut codes: Vec<&'static str> = Vec::new();
    let mut saw_local = false;
    let mut only_builtin = true;
    for pass in [
        ProviderTestStatus::Ok,
        ProviderTestStatus::Untested,
        ProviderTestStatus::Failed,
    ] {
        for provider in &enabled {
            if provider.test_status != pass {
                continue;
            }
            let is_local = adapter::adapter_for(&provider.adapter_id)
                .map(|adapter| adapter.is_local())
                .unwrap_or(false);
            if provider.id != local_engine::BUILTIN_LOCAL_ASR_ID {
                only_builtin = false;
            }
            match evaluate(core, provider, capability)? {
                Ok(()) => {
                    return Ok(CapabilityGate::ready(
                        &provider.id,
                        if is_local {
                            format!(
                                "{}（本机引擎 {}，模型目录 {}）可承担 {}",
                                provider.label,
                                provider.adapter_id,
                                provider.model.as_deref().unwrap_or("<随包/环境变量解析>"),
                                capability.wire()
                            )
                        } else {
                            format!(
                                "服务「{}」（{}，{}）可承担 {}",
                                provider.label,
                                provider.adapter_id,
                                provider
                                    .endpoint
                                    .as_deref()
                                    .map(endpoint_scope)
                                    .unwrap_or("未配 endpoint"),
                                capability.wire()
                            )
                        },
                    ));
                }
                Err(reason) => {
                    saw_local |= is_local;
                    hints.push(reason);
                    codes.push(blocked_code(
                        provider_last_error(&core.conn, &provider.id)?.as_deref(),
                    ));
                }
            }
        }
    }

    // 一个配置行都没有、只有内建引擎时，别写成「已启用的 1 个服务」——那不是
    // 用户配的服务，名字也别绕。
    let reason = if only_builtin {
        format!(
            "{}不可用，因此现在不能用 {}：{}",
            local_engine::BUILTIN_LOCAL_ASR_LABEL,
            capability.wire(),
            hints.join("；")
        )
    } else {
        format!(
            "已启用的 {} 个服务都不能承担 {}：{}",
            enabled.len(),
            capability.wire(),
            hints.join("；")
        )
    };
    // 有一个是鉴权失败就报鉴权失败：那是最具体的、用户能直接动手的原因。
    let code = if codes.contains(&"authentication_failed") {
        "authentication_failed"
    } else {
        "provider_not_configured"
    };
    Ok(CapabilityGate::blocked(
        reason,
        if saw_local {
            local_next_step(capability)
        } else {
            next_step("补上这个能力")
        },
        code,
    ))
}

/// 服务最近一次失败的错误码；没失败过就是 None。
fn provider_last_error(conn: &rusqlite::Connection, provider_id: &str) -> Result<Option<String>> {
    let row: Option<Option<String>> = conn
        .query_row(
            "SELECT last_error_code FROM providers WHERE id = ?1",
            params![provider_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(row.flatten())
}

/// 等配置时写进作业的错误码：鉴权失败就如实写鉴权失败。
fn blocked_code(last_error: Option<&str>) -> &'static str {
    match last_error {
        Some("authentication_failed") => "authentication_failed",
        _ => "provider_not_configured",
    }
}

// ------------------------------------------------------------------ 连接测试

/// 连接测试，契约第 4.4 节 `providers.test`。
///
/// # 什么时候真的发请求
///
/// 只在这个方法里、且只在用户显式给出的 `scope` 上发。每一项能力要么是「配置/
/// 适配器层面就不成立」（直接报 failed，**不发请求**），要么发一次真实的探针调用。
///
/// # 报告里有什么
///
/// - `results`：逐能力的结论（ok / failed）；
/// - `actual_call_made`：**数出来的**真实调用次数是否大于 0；
/// - `message`：可达性 / 认证 / 模型 / 实际能力集 / 端点是本机还是远端 / 下一步。
///   **一次调用都没发时不说「可达 / 认证通过 / 模型可用」**——那三项没人验过，报告
///   如实写「未发起调用、都未验证」（#96）。
///
/// 认证失败、超时这类**拿到了明确失败原因**的情况按契约走错误码（可区分的
/// `authentication_failed` / `provider_timeout` …），错误消息里同样写明「本次
/// 测试已发起 N 次真实请求」。数据库里的 `test_status` / `last_error_code` 也会
/// 落成失败，界面重开时看得到。
pub(crate) fn test(
    core: &mut Core,
    provider_id: &str,
    scope: &[ProviderCapability],
) -> Result<ProviderTestReport> {
    if scope.is_empty() {
        return Err(CoreError::InvalidArgument {
            reason: "连接测试要明确用户授权的测试范围（至少一项能力）".to_owned(),
        });
    }
    // 内建的本机引擎也能测：它才是「转写到底能不能用」的真相来源，
    // 却不在 `providers` 表里（见 `local_engine` 的模块文档）。这一条特例是
    // 刻意的：测试路径**不影响**「默认不联网」，因为它一个字节都不出网。
    let provider = if provider_id == local_engine::BUILTIN_LOCAL_ASR_ID
        && load_provider(&core.conn, provider_id).is_err()
    {
        local_engine::builtin_provider()
    } else {
        load_provider(&core.conn, provider_id)?
    };
    let adapter = adapter::adapter_for(&provider.adapter_id).ok_or_else(|| {
        CoreError::ProviderNotConfigured {
            reason: format!(
                "服务「{}」的适配器「{}」不在本构建里",
                provider.label, provider.adapter_id
            ),
        }
    })?;
    if adapter.is_local() {
        return test_local(core, &provider, &adapter, scope);
    }
    // 走到这里的只可能是 HTTP 适配器（本机引擎在上面就返回了）。
    let adapter = match adapter {
        adapter::Adapter::Http(adapter) => adapter,
        adapter::Adapter::Local(_) => {
            return Err(CoreError::InvalidState {
                entity: "服务配置",
                id: provider_id.to_owned(),
                state: "本机引擎的连接测试应当走 test_local 分支".to_owned(),
            })
        }
    };
    let endpoint = provider
        .endpoint
        .as_deref()
        .ok_or_else(|| CoreError::ProviderNotConfigured {
            reason: format!("服务「{}」还没有配置 endpoint", provider.label),
        })?;
    let secret = provider_secret(core, &provider)?.ok_or_else(|| {
        CoreError::ProviderNotConfigured {
            reason: format!("服务「{}」还没有可用的密钥", provider.label),
        }
    })?;

    let mut results = std::collections::HashMap::new();
    let mut notes: Vec<String> = Vec::new();
    let mut calls: usize = 0;
    // 这一次真的探过、而且过了的能力（失败时也要如实保留它们的结论）。
    let mut probed_ok: Vec<ProviderCapability> = Vec::new();

    // 范围去重 + 稳定顺序：报告与数据库的写入顺序不受调用方集合顺序影响。
    let mut wanted: Vec<ProviderCapability> = Vec::new();
    for capability in ProviderCapability::ALL {
        if scope.contains(&capability) {
            wanted.push(capability);
        }
    }

    for capability in wanted {
        // 适配器是实现层的事实，先看它：没实现的能力，声明了也不算数。
        if !adapter.capabilities().contains(&capability) {
            results.insert(capability, ProviderTestStatus::Failed);
            notes.push(format!(
                "{}：适配器「{}」没有实现，未发起调用",
                capability.wire(),
                provider.adapter_id
            ));
            continue;
        }
        if !provider.capabilities.contains(&capability) {
            results.insert(capability, ProviderTestStatus::Failed);
            notes.push(format!(
                "{}：配置里没有声明这个能力，未发起调用",
                capability.wire()
            ));
            continue;
        }

        let request = adapter.probe(&provider, &secret, capability)?;
        let outcome = core.transport().send(&request);
        calls += 1;

        let response = match outcome {
            Ok(response) => response,
            Err(error) => {
                let error = normalize::transport_error(&error, Some(&secret));
                record_test_failure(core, provider_id, error.code().wire(), &probed_ok, capability)?;
                return Err(with_call_note(error, calls));
            }
        };
        if !(200..300).contains(&response.status) {
            let error = normalize::http_error(&response, Some(&secret));
            record_test_failure(core, provider_id, error.code().wire(), &probed_ok, capability)?;
            return Err(with_call_note(error, calls));
        }
        if let Err(error) = adapter.parse_probe(capability, &response, Some(&secret)) {
            record_test_failure(core, provider_id, error.code().wire(), &probed_ok, capability)?;
            return Err(with_call_note(error, calls));
        }

        results.insert(capability, ProviderTestStatus::Ok);
        probed_ok.push(capability);
        notes.push(format!("{}：验证通过", capability.wire()));
    }

    let verified = probed_ok.clone();
    let all_ok = results.values().all(|status| *status == ProviderTestStatus::Ok);
    // 范围里有不成立的能力：状态落 failed，错误码用契约里的
    // `provider_not_configured`（「没有可用配置能满足这次请求的能力」），
    // 不自己编一个新码——`last_error_code` 列里只放契约第 7 节的码。
    let error_code = if all_ok {
        None
    } else {
        Some("provider_not_configured")
    };
    record_test_outcome(core, provider_id, &probed_ok, &verified, all_ok, error_code)?;

    // 报告口径：**只有真的发过调用**，才谈得上「端点可达 / 认证通过 / 模型可用」。
    // 一次都没发（范围里全是适配器没实现、或配置没声明的能力）时如实说这三项都没验过——
    // 否则报告会写出一句谁都没验过的「端点/认证/模型均已验证」（#96 评审的第三条）。
    let verified_text = if verified.is_empty() {
        "（本次范围内没有验证通过的能力）".to_owned()
    } else {
        verified
            .iter()
            .map(|item| item.wire())
            .collect::<Vec<_>>()
            .join("、")
    };
    let mut message = if calls == 0 {
        format!(
            "本次测试**没有发起任何真实调用**（范围里没有「适配器实现了且配置声明了」的\
             能力），所以端点连通性、认证、模型可用性**都没验过**；\
             实际验证通过的能力集：{verified_text}"
        )
    } else {
        format!(
            "端点 {endpoint}（{}）可达；认证通过；模型 {} 可用；实际能力集：{verified_text}；\
             本次测试发起了 {calls} 次真实请求（可能计费）",
            endpoint_scope(endpoint),
            provider.model.as_deref().unwrap_or("<未配置>"),
        )
    };
    if !notes.is_empty() {
        message.push('；');
        message.push_str(&notes.join("；"));
    }
    if !all_ok {
        message.push_str(
            "。下一步：按上面的原因修配置（声明能力 / 换支持它的适配器 / 换模型），再测一次",
        );
    }

    Ok(ProviderTestReport {
        provider_id: provider_id.to_owned(),
        results,
        actual_call_made: calls > 0,
        message: Some(message),
    })
}

/// 本机引擎的连接测试：**真的加载模型并转写一小段静音**，但不联网。
///
/// 与 HTTP 那条路的语义对齐：
/// - 配置/适配器层面就不成立的能力**不做任何调用**，直接报 failed 并写明原因；
/// - 范围内的能力会跑一次真实的本地探针（加载模型 + 0.05 秒静音推理）；
/// - `actual_call_made` 如实表示「真的跑了一次推理」——它不是「发了请求」，
///   契约里这个字段的含义是「本次测试发生了真实调用」，本地推理也算；
/// - 结论按能力落库（`verified_capabilities`），失败时把这次探过的能力摘掉。
///
/// 报告里必须写清**没有联网**：用户看到「连接测试通过」时的第一反应是
/// 「我的资料被发出去了吗」，这句话就是回答。
fn test_local(
    core: &mut Core,
    provider: &Provider,
    adapter: &adapter::Adapter,
    scope: &[ProviderCapability],
) -> Result<ProviderTestReport> {
    let mut wanted: Vec<ProviderCapability> = Vec::new();
    for capability in ProviderCapability::ALL {
        if scope.contains(&capability) {
            wanted.push(capability);
        }
    }

    let mut results = std::collections::HashMap::new();
    let mut notes: Vec<String> = Vec::new();
    let mut probed: Vec<ProviderCapability> = Vec::new();
    for capability in &wanted {
        if !adapter.capabilities().contains(capability) {
            results.insert(*capability, ProviderTestStatus::Failed);
            notes.push(format!(
                "{}：本机引擎「{}」没有实现，未做任何调用",
                capability.wire(),
                provider.adapter_id
            ));
            continue;
        }
        if !provider.capabilities.contains(capability) {
            results.insert(*capability, ProviderTestStatus::Failed);
            notes.push(format!("{}：配置里没有声明这个能力，未做任何调用", capability.wire()));
            continue;
        }
        probed.push(*capability);
    }

    let mut calls = 0_usize;
    let mut probe_note = None;
    let mut failure: Option<CoreError> = None;
    if !probed.is_empty() {
        match local_engine::config_for(core, provider.model.as_deref()) {
            Ok(config) => {
                // 一次探针覆盖范围内所有本机能力：它验的是「同一个引擎能不能跑」。
                match core.asr_engine().probe(&config) {
                    Ok(message) => {
                        calls = 1;
                        probe_note = Some(message);
                    }
                    Err(error) => failure = Some(error),
                }
            }
            Err(error) => failure = Some(error),
        }
    }

    if let Some(error) = failure {
        let code = error.code().wire();
        // 这次要探的第一个能力从「验过」里摘掉：探针失败（含配置解析不出来）说明
        // 这个引擎现在不可信，不能继续拿着上一次的结论排任务。
        let failing = probed.first().copied();
        record_test_failure(core, &provider.id, code, &[], failing.unwrap_or(ProviderCapability::TranscribeAudio))?;
        let mut error = error;
        error.append_reason("；本次测试**没有联网**（本机引擎）");
        return Err(error);
    }

    for capability in &probed {
        results.insert(*capability, ProviderTestStatus::Ok);
        notes.push(format!("{}：本机推理验证通过（未联网）", capability.wire()));
    }
    let all_ok = results.values().all(|status| *status == ProviderTestStatus::Ok);
    // 逐能力结论已经落在 `results` 里，`verified` 就是「这次真的探过、而且过了」的那些。
    let verified = probed.clone();
    record_test_outcome(
        core,
        &provider.id,
        &probed,
        &verified,
        all_ok,
        if all_ok {
            None
        } else {
            Some("provider_not_configured")
        },
    )?;

    let mut message = probe_note.unwrap_or_else(|| {
        format!(
            "本机引擎「{}」：本次范围内没有可验证的能力，未做任何调用",
            provider.adapter_id
        )
    });
    message.push_str(&format!(
        "；模型目录 {}；本次测试完成了 {calls} 次本机推理，**没有发起任何网络请求**",
        provider.model.as_deref().unwrap_or("<随包/环境变量解析>")
    ));
    if !notes.is_empty() {
        message.push('；');
        message.push_str(&notes.join("；"));
    }
    if !all_ok {
        message.push_str("。下一步：按上面的原因修配置（模型目录 / 声明能力）再测一次");
    }

    Ok(ProviderTestReport {
        provider_id: provider.id.clone(),
        results,
        actual_call_made: calls > 0,
        message: Some(message),
    })
}

/// 错误消息里补一句「发生过几次真实调用」——契约的报告要说明这件事，
/// 而失败路径走的是错误码，消息就是它唯一能带话的地方。
fn with_call_note(error: CoreError, calls: usize) -> CoreError {
    let mut error = error;
    error.append_reason(&format!("；本次测试已发起 {calls} 次真实请求（可能计费）"));
    error
}

fn record_test_failure(
    core: &mut Core,
    provider_id: &str,
    error_code: &str,
    probed_ok: &[ProviderCapability],
    failing: ProviderCapability,
) -> Result<()> {
    // 结论按能力算清楚：
    // - 这一次真的探过、过了的：如实记成「验过」（它们确实被验过）；
    // - 这次正在探、探失败了的：从「验过」里摘掉——闸门认「测过就要验过」，
    //   留着旧结论会让一个刚被证明坏掉的能力继续被排任务；
    // - 范围外没碰过的：保留上一次的结论（用户只授权测一项，不该被连坐）。
    let mut current = verified_capabilities(&core.conn, provider_id)?;
    for capability in probed_ok {
        current.insert(*capability);
    }
    current.remove(&failing);
    let now_text = support::to_iso(now());
    core.conn.execute(
        "UPDATE providers SET test_status = 'failed', last_error_code = ?1, last_tested_at = ?2, \
         verified_capabilities = ?3, updated_at = ?2 WHERE id = ?4",
        params![error_code, now_text, capabilities_json(&current)?, provider_id],
    )?;
    promote_configuration_waits(core, now())?;
    Ok(())
}

/// 把这次测试的结论落库：**只改这次真的探过的能力**，范围外保留旧结论。
///
/// 这是刻意的：用户只授权测 `generate_text` 时，不该把他之前验过的
/// `transcribe_audio` 一起抹掉。
fn record_test_outcome(
    core: &mut Core,
    provider_id: &str,
    probed: &[ProviderCapability],
    verified: &[ProviderCapability],
    all_ok: bool,
    error_code: Option<&str>,
) -> Result<()> {
    let mut current = verified_capabilities(&core.conn, provider_id)?;
    for capability in probed {
        if verified.contains(capability) {
            current.insert(*capability);
        } else {
            current.remove(capability);
        }
    }
    let status = if all_ok {
        ProviderTestStatus::Ok
    } else {
        ProviderTestStatus::Failed
    };
    let now_text = support::to_iso(now());
    core.conn.execute(
        "UPDATE providers SET verified_capabilities = ?1, test_status = ?2, last_error_code = ?3, \
         last_tested_at = ?4, updated_at = ?4 WHERE id = ?5",
        params![
            capabilities_json(&current)?,
            status.wire(),
            error_code,
            now_text,
            provider_id
        ],
    )?;
    promote_configuration_waits(core, now())?;
    Ok(())
}

/// 执行期发现这个服务的配置坏了（鉴权失败、模型/端点不可用）时，把它的结论
/// 如实改成失败。
///
/// 为什么必须做：闸门认「测过就要验过」，所以这一笔会让等在 `waiting_configuration`
/// 的任务**真的停下来等用户**，而不是每次唤醒都去撞一次 401（契约第 7 节：
/// 等待配置，不频繁重试）。超时与断网不改结论——那是暂时性的，不该让用户重测。
pub(crate) fn mark_provider_verdict_failed(
    core: &mut Core,
    provider_id: &str,
    error_code: &str,
) -> Result<()> {
    if !matches!(error_code, "authentication_failed" | "model_unavailable") {
        return Ok(());
    }
    let now_text = support::to_iso(now());
    core.conn.execute(
        "UPDATE providers SET test_status = 'failed', verified_capabilities = '[]', \
         last_error_code = ?1, last_tested_at = ?2, updated_at = ?2 WHERE id = ?3",
        params![error_code, now_text, provider_id],
    )?;
    Ok(())
}

// ------------------------------------------------------------------ 主动回应

/// `responses.request`：只有**用户主动调用**才排任务，契约第 4.3 节。
///
/// 幂等口径：同一个（请求文字 + 来源集合 + 服务）在还有**活着的**回应任务时，
/// 直接返回那条任务与它对应的回应，不新建、不再排。契约的 Dart 签名没有
/// operationId（`requestResponse` 不接受它），所以这里用内容做幂等键。
pub(crate) fn request_response(
    core: &mut Core,
    capture_ids: &[String],
    request_text: &str,
    provider_id: Option<&str>,
) -> Result<ResponseTicket> {
    let request_text = request_text.trim();
    if request_text.is_empty() {
        return Err(CoreError::InvalidArgument {
            reason: "请求文字是空的；主动回应需要用户真的说了点什么".to_owned(),
        });
    }
    for capture_id in capture_ids {
        let exists: i64 = core.conn.query_row(
            "SELECT COUNT(*) FROM captures WHERE id = ?1",
            params![capture_id],
            |row| row.get(0),
        )?;
        if exists == 0 {
            return Err(CoreError::NotFound {
                entity: "记录",
                id: capture_id.clone(),
            });
        }
    }

    // 明确点了名却不存在，是调用方的错（`not_found`），不是「等配置」。
    if let Some(provider_id) = provider_id {
        load_provider(&core.conn, provider_id)?;
    }

    // 能力闸门：不可用就**不排任务**（排一条注定失败的任务对谁都没好处）。
    let gate = capability_gate_for(core, ProviderCapability::GenerateText, provider_id)?;
    if !gate.ready {
        return Err(CoreError::ProviderNotConfigured {
            reason: match &gate.next_step {
                Some(next) => format!("{}；{}", gate.reason, next),
                None => gate.reason.clone(),
            },
        });
    }
    let chosen = gate
        .provider_id
        .clone()
        .expect("闸门说可用就一定有选中的服务");
    let provider = load_provider(&core.conn, &chosen)?;

    let mut parts: Vec<String> = vec!["responses.request".to_owned(), request_text.to_owned()];
    parts.extend(capture_ids.iter().cloned());
    parts.push(chosen.clone());
    let fingerprint = support::fingerprint(&parts.iter().map(String::as_str).collect::<Vec<_>>());

    // 还有活着的同内容任务就直接复用：它的回应行 id 在 target_ids 里。
    let existing: Option<(String, String)> = core
        .conn
        .query_row(
            "SELECT id, target_ids FROM jobs WHERE kind = ?1 AND input_snapshot_hash = ?2 \
             AND state NOT IN ('succeeded', 'failed', 'cancelled') \
             ORDER BY created_at, id LIMIT 1",
            params![KIND_RESPOND, fingerprint],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((job_id, target_ids)) = existing {
        let response_id: Option<String> = serde_json::from_str::<Vec<String>>(&target_ids)
            .ok()
            .and_then(|ids| ids.into_iter().next());
        return Ok(ResponseTicket { job_id, response_id });
    }

    let response_id = support::new_id("rsp");
    let now_text = support::to_iso(now());
    let captures_json = serde_json::to_string(capture_ids)?;
    let job = NewJob::new(KIND_RESPOND, RESPOND_PRIORITY)
        .targeting(vec![response_id.clone()])
        .with_snapshot(fingerprint);

    let tx = core.conn.transaction()?;
    tx.execute(
        "INSERT INTO ai_responses (id, request_text, capture_ids, provider_id, model, body, state, \
         error_code, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, '', 'pending', NULL, ?6, ?6)",
        params![
            response_id,
            request_text,
            captures_json,
            provider.id,
            provider.model,
            now_text
        ],
    )?;
    // `job.changed` 由队列在入队时发（`enqueue_in_tx`），这里不重复发。
    let queued = crate::jobs::enqueue_in_tx(&tx, job)?;
    tx.commit()?;

    Ok(ResponseTicket {
        job_id: queued.id,
        response_id: Some(response_id),
    })
}

/// `ai_responses` 读出的一行（解析前先按位置取出来）。
type RawResponse = (
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
);

pub(crate) fn get_response(core: &Core, response_id: &str) -> Result<AiResponse> {
    let raw: Option<RawResponse> = core
        .conn
        .query_row(
            "SELECT request_text, capture_ids, provider_id, model, body, created_at, state \
             FROM ai_responses WHERE id = ?1",
            params![response_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((request_text, capture_ids, provider_id, model, body, created_at, _state)) = raw else {
        return Err(CoreError::NotFound {
            entity: "回应",
            id: response_id.to_owned(),
        });
    };
    Ok(AiResponse {
        id: response_id.to_owned(),
        request_text,
        body,
        created_at: support::parse_iso(&created_at)?,
        capture_ids: serde_json::from_str(&capture_ids)?,
        provider_id,
        model,
    })
}

/// `respond` 作业的执行者：真的调用一次模型，把正文落库。
///
/// **只在被领取执行时调用**——队列不动，就没有请求。已经在跑的请求重入时
/// （租约恢复）直接返回：正文已在，不重复调用、不重复计费。
pub(crate) fn run_respond(core: &mut Core, job: &Job) -> std::result::Result<(), CoreError> {
    // 目标一定在：空目标由驱动在派发前就按 `no_target` 终态失败（见 `jobs::execute`）。
    let Some(response_id) = job.target_ids.first().cloned() else {
        return Err(CoreError::InvalidState {
            entity: "任务",
            id: job.id.clone(),
            state: format!("{ERROR_NO_TARGET}：回应任务没有目标"),
        });
    };
    let outcome = respond_once(core, &response_id);
    if let Err(error) = &outcome {
        let _ = mark_response_error(core, &response_id, error.code().wire());
    }
    outcome
}

fn respond_once(core: &mut Core, response_id: &str) -> std::result::Result<(), CoreError> {
    // 只取这一片真的用得上的四样：请求文字、状态、钉住的服务、模型名由适配器决定。
    // `capture_ids` 是来源记录（B4b 拼上下文时才用），这里不读。
    let row: Option<(String, String, Option<String>)> = core
        .conn
        .query_row(
            "SELECT request_text, state, provider_id FROM ai_responses WHERE id = ?1",
            params![response_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((request_text, state, provider_id)) = row else {
        return Err(CoreError::NotFound {
            entity: "回应",
            id: response_id.to_owned(),
        });
    };
    if state == "ready" {
        // 已经生成过：重入不该再花一次钱。
        return Ok(());
    }

    let gate = capability_gate_for(core, ProviderCapability::GenerateText, provider_id.as_deref())?;
    if !gate.ready {
        return Err(CoreError::ProviderNotConfigured {
            reason: match &gate.next_step {
                Some(next) => format!("{}；{}", gate.reason, next),
                None => gate.reason.clone(),
            },
        });
    }
    let provider = load_provider(
        &core.conn,
        gate.provider_id
            .as_deref()
            .expect("闸门说可用就一定有选中的服务"),
    )?;
    let adapter = adapter::adapter_for(&provider.adapter_id).ok_or_else(|| {
        CoreError::ProviderNotConfigured {
            reason: format!("服务「{}」的适配器不在本构建里", provider.label),
        }
    })?;
    // 本地引擎没有「生成文本」这个能力（`capabilities()` 里就没有它），所以闸门
    // 不可能把 `respond` 指到本机引擎上。这里再挡一次是防御：绕过闸门也不能走进
    // 一个不存在的 HTTP 调用。
    let adapter = match adapter {
        adapter::Adapter::Http(adapter) => adapter,
        adapter::Adapter::Local(engine) => {
            return Err(CoreError::ProviderNotConfigured {
                reason: format!(
                    "服务「{}」是本机引擎「{}」，它不实现 generate_text；\
                     主动回应要配一个支持 generate_text 的服务",
                    provider.label,
                    engine.id()
                ),
            })
        }
    };
    let secret = provider_secret(core, &provider)?.ok_or_else(|| {
        CoreError::ProviderNotConfigured {
            reason: format!("服务「{}」还没有可用的密钥", provider.label),
        }
    })?;

    // 提示词与上下文拼装是 B4b 的事（风格、快照、来源取舍）。这一片如实只把
    // 用户自己的请求文字发出去，`capture_ids` 作为来源记录下来，不擅自编提示词。
    let request = adapter.generate(
        &provider,
        &secret,
        &adapter::GenerateRequest {
            prompt: request_text,
            system: None,
        },
    )?;
    let response = core
        .transport()
        .send(&request)
        .map_err(|error| normalize::transport_error(&error, Some(&secret)))?;
    if !(200..300).contains(&response.status) {
        let error = normalize::http_error(&response, Some(&secret));
        mark_provider_verdict_failed(core, &provider.id, error.code().wire())?;
        return Err(error);
    }
    let outcome = adapter.parse_generate(&response, Some(&secret))?;

    let now_text = support::to_iso(now());
    core.conn.execute(
        "UPDATE ai_responses SET body = ?1, model = COALESCE(?2, model), state = 'ready', \
         error_code = NULL, updated_at = ?3 WHERE id = ?4",
        params![outcome.text, outcome.model, now_text, response_id],
    )?;
    Ok(())
}

fn mark_response_error(core: &mut Core, response_id: &str, error_code: &str) -> Result<()> {
    core.conn.execute(
        "UPDATE ai_responses SET error_code = ?1, updated_at = ?2 WHERE id = ?3",
        params![error_code, support::to_iso(now()), response_id],
    )?;
    Ok(())
}

// ------------------------------------------------------------------ 队列侧的配置闸门

/// 作业 kind → 需要的能力。
///
/// **只列真有执行者、而且执行者真的会调 Provider 的 kind**。B4a 留这个映射点时
/// `transcribe` 还没有执行者，所以没有列它（队列对没有执行者的 kind 走诚实的
/// `no_handler`）；#91 把执行者接上了，`transcribe` 于是也能在这里被闸门挡住——
/// 引擎/模型没就绪时任务停在 `waiting_configuration`，不会先跑一次再失败。
/// `diary_generate`（B4b 的日记生成）与 `respond` 一样需要 `generate_text`，
/// 也在**领取前**就被这条闸门挡住（`waiting_configuration`），配置好了再跑——
/// 这正是「没配服务也不算调用失败」的落地。
///
/// 几条**能力需求**不同：
/// - `respond` / `diary_generate` 需要 `generate_text`（HTTP 服务）；
/// - `transcribe` 需要 `transcribe_audio`，而且录音这条链**只跑本机引擎**
///   （见 `job_gate` 的 `local_only`）。
pub(crate) fn job_capability(kind: &str) -> Option<ProviderCapability> {
    match kind {
        KIND_RESPOND => Some(ProviderCapability::GenerateText),
        kind if kind == crate::diary::KIND_DIARY_GENERATE => Some(ProviderCapability::GenerateText),
        crate::transcription::KIND_TRANSCRIBE => Some(ProviderCapability::TranscribeAudio),
        _ => None,
    }
}

/// 一条作业能不能现在就被交给 Provider。返回 `None` 表示这个 kind 不依赖 Provider。
pub(crate) fn job_gate(core: &Core, job: &Job) -> Result<Option<CapabilityGate>> {
    let Some(capability) = job_capability(&job.kind) else {
        return Ok(None);
    };
    // `respond` / `diary_generate` 在请求受理时就把服务钉死了：判定要与执行者用同一个
    // 服务，否则会出现「闸门说可以 → 执行者说不行 → 每次唤醒都撞一次」的抖动。
    // 受理时没有可用服务（`provider_id` 为空）时两边都按「挑一个」的同一次调用判定。
    let pinned = match job.kind.as_str() {
        KIND_RESPOND => job
            .target_ids
            .first()
            .map(|response_id| pinned_provider(core, response_id))
            .transpose()?
            .flatten(),
        kind if kind == crate::diary::KIND_DIARY_GENERATE => job
            .target_ids
            .first()
            .map(|generation_id| crate::diary::pinned_provider(core, generation_id))
            .transpose()?
            .flatten(),
        _ => None,
    };
    // 转写这一路**只认本机引擎**：录音的转写链跑的是本机 ONNX 引擎
    // （`transcription` 模块），远端服务不在它的执行范围里——闸门不能说
    // 「可用」然后执行者用另一个东西跑。
    let local_only = job.kind == crate::transcription::KIND_TRANSCRIBE;
    Ok(Some(capability_gate_for_with(
        core,
        capability,
        pinned.as_deref(),
        local_only,
    )?))
}

fn pinned_provider(core: &Core, response_id: &str) -> Result<Option<String>> {
    let provider_id: Option<Option<String>> = core
        .conn
        .query_row(
            "SELECT provider_id FROM ai_responses WHERE id = ?1",
            params![response_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(provider_id.flatten())
}

/// 配置变了以后重评队列：能跑的放回 `queued`，不能跑的停在
/// `waiting_configuration` 并写清「在等什么」。
///
/// 与依赖闸门同一形状（`jobs::resolve_dependencies`）：**领取前**评估，所以
/// 等配置的任务**不会**消耗尝试次数（没跑过就不计尝试），也不会把「没有服务」
/// 说成「失败」。但它**要置 `requires_user_action = 1` 并写
/// `attention_key = job-config:<kind>:<code>`**：等配置不是失败，但**不是一个用户
/// 无需理会的状态**——用户得去把服务配上。这一句跨 kind 一把尺（语义索引等模型的
/// 闸门 `jobs::defer_for_configuration` 与执行期那条
/// `jobs::defer_for_configuration_after_attempt` 给同一个答案），key 的形状也只有
/// 一处（`jobs::configuration_attention_key`）。
/// 每一次 Provider 配置变更（保存 / 移除 / 连接测试）都会调用它一次。
pub(crate) fn promote_configuration_waits(core: &mut Core, at: DateTime<Utc>) -> Result<i64> {
    let jobs: Vec<Job> = {
        let mut statement = core.conn.prepare(
            "SELECT id FROM jobs WHERE state IN ('queued', 'retry_wait', 'waiting_configuration') \
             ORDER BY created_at, id",
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut jobs = Vec::new();
        for id in ids {
            jobs.push(crate::jobs::load_job(&core.conn, &id)?);
        }
        jobs
    };

    let now_text = support::to_iso(at);
    let mut changed = 0;
    for job in jobs {
        let Some(gate) = job_gate(core, &job)? else {
            continue;
        };
        let waiting = job.state == crate::model::JobState::WaitingConfiguration;
        if gate.ready {
            if waiting {
                core.conn.execute(
                    "UPDATE jobs SET state = 'queued', error_code = NULL, next_attempt_at = NULL, \
                     requires_user_action = 0, attention_key = NULL, updated_at = ?1 WHERE id = ?2",
                    params![now_text, job.id],
                )?;
                let tx = core.conn.transaction()?;
                crate::insert_event(&tx, crate::model::EventType::JobChanged, &job.id, 1)?;
                tx.commit()?;
                changed += 1;
            }
            continue;
        }

        let code = gate.code.as_str();
        // `attention_key` 的形状只有一处（三条写入者共用），界面靠它深链到「配哪个服务」。
        let attention = crate::jobs::configuration_attention_key(&job.kind, code);
        if !waiting || job.error_code.as_deref() != Some(code) {
            core.conn.execute(
                "UPDATE jobs SET state = 'waiting_configuration', error_code = ?1, \
                 next_attempt_at = NULL, requires_user_action = 1, attention_key = ?2, \
                 updated_at = ?3 WHERE id = ?4",
                params![code, attention, now_text, job.id],
            )?;
            let tx = core.conn.transaction()?;
            crate::insert_event(&tx, crate::model::EventType::JobChanged, &job.id, 1)?;
            tx.commit()?;
            changed += 1;
        }
        // 「在等什么」的完整理由不塞进数据库：它由能力闸门**实时**给出
        // （`CapabilityGate` 的 reason / next_step），不会随配置变化过期，也不会
        // 在库里留下一句与现状不符的话。任务面板拿到 `provider_not_configured` /
        // `authentication_failed` 时，用闸门解释这句话（见 m3 文档「没有做到的」：
        // 这个解释还没有桥接出去，是这一片留下的缺口）。
    }
    Ok(changed)
}