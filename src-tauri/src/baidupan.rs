//! 百度网盘音源：网页扫码登录（BDUSS 会话）+ 目录浏览 + 递归导入 + 直链播放。
//!
//! 与网易云 / QQ / 酷狗这类"曲库"音源不同，网盘没有全网检索接口，只能读取用户
//! 自己网盘里的文件，因此模型是：
//! 「选择网盘里的一个文件夹 → 递归扫出音频文件 → 按目录结构生成专辑 → 按专辑播放」。
//!
//! ## 为什么是扫码登录而不是开放平台 OAuth
//! 开放平台 OAuth 要求用户先在开发者控制台创建应用、拿 AppKey/AppSecret，再把授权码
//! 从浏览器复制回来，门槛高。这里改成：在应用内打开百度登录页（登录页默认就是扫码），
//! 用户用手机百度 App 扫码后，我们从 WebView 的 cookie 仓库里取 `BDUSS`，随后所有
//! 网盘请求都带这个会话 cookie——与网页版 / 各类网盘客户端同一套鉴权，无需注册应用。
//!
//! ## 播放
//! 网盘文件不能直接当 URL 播（直链是限时签名地址，且必须带 `User-Agent: pan.baidu.com`），
//! 每次播放现取直链，下载/缓存链路复用 `engine::cache`（kind == "baidu" 时补该 UA）。

use crate::credentials;
use crate::db;
use crate::AppState;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// DB / 前端统一的音源标识
pub const KIND: &str = "baidu";

const WEB_BASE: &str = "https://pan.baidu.com";
const WEB_LIST: &str = "https://pan.baidu.com/api/list";
const WEB_TEMPLATE_VARS: &str = "https://pan.baidu.com/api/gettemplatevariable";
const WEB_FILEMETAS: &str = "https://pan.baidu.com/api/filemetas";
const XPAN_MULTIMEDIA: &str = "https://pan.baidu.com/rest/2.0/xpan/multimedia";

/// 下载接口要求：请求头 UA 必须是 pan.baidu.com，否则 403
pub const DOWNLOAD_UA: &str = "pan.baidu.com";
/// 网页接口要浏览器 UA，否则可能被风控
const WEB_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

const K_ROOT: &str = "baidu_roots";
const K_USER: &str = "baidu_user";
const K_STOKEN: &str = "baidu_stoken";
/// bdstoken 在凭据文件里再存一份：下载线程拿不到 DB 连接，但 dlink 下载要带它
const C_STOKEN: &str = "baidu_stoken";
/// 会话 cookie 走凭据存储（macOS = 本机 0600 文件，Windows/Linux = 系统凭据管理器）
const C_BDUSS: &str = "baidu_bduss";

/// 与本地资料库同口径的可解码扩展名（symphonia 侧没有 ape/wma 解码器，导入也会播不了）
// 只收真正的音乐容器：mp4/m4b 是视频/有声书容器，网盘目录里常混着视频
const AUDIO_EXTS: [&str; 7] = ["mp3", "flac", "wav", "ogg", "oga", "m4a", "aac"];

/// 会话失效：内部信号，with_session 捕获后提示重新登录
const SESSION_EXPIRED: &str = "__baidu_session_expired__";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn q(v: &str) -> String {
    utf8_percent_encode(v, NON_ALPHANUMERIC).to_string()
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .user_agent(WEB_UA)
        .build()
}

// ---------- 会话（BDUSS + bdstoken） ----------

#[derive(Clone)]
pub struct Session {
    pub bduss: String,
    pub stoken: String,
}

fn cookie_header(s: &Session) -> String {
    if s.stoken.is_empty() {
        format!("BDUSS={}", s.bduss)
    } else {
        format!("BDUSS={}; STOKEN={}", s.bduss, s.stoken)
    }
}

pub fn session() -> Option<Session> {
    let bduss = credentials::get(C_BDUSS).unwrap_or_default();
    if bduss.trim().is_empty() {
        return None;
    }
    Some(Session {
        bduss: bduss.trim().to_string(),
        stoken: String::new(),
    })
}

pub fn authorized() -> bool {
    session().is_some()
}

pub fn user_name(conn: &Connection) -> String {
    db::get_setting(conn, K_USER).unwrap_or_default()
}

pub fn logout(conn: &Connection) {
    credentials::clear(C_BDUSS);
    db::set_setting(conn, K_STOKEN, "");
    db::set_setting(conn, K_USER, "");
}

/// 登录成功后落盘：BDUSS 进凭据存储，bdstoken / 昵称进 settings
pub fn save_login(conn: &Connection, bduss: &str, stoken: &str, user: &str) -> Result<(), String> {
    let bduss = bduss.trim();
    if bduss.is_empty() {
        return Err("没有拿到百度登录态（BDUSS），请重试".into());
    }
    credentials::set(C_BDUSS, bduss)?;
    let _ = credentials::set(C_STOKEN, stoken);
    db::set_setting(conn, K_STOKEN, stoken);
    db::set_setting(conn, K_USER, user);
    Ok(())
}

/// 用现有 BDUSS 换 bdstoken / 昵称（网页接口的多数请求都要 bdstoken）
pub fn sync_profile(conn: &Connection) -> Result<String, String> {
    let mut s = session().ok_or("尚未登录百度网盘")?;
    s.stoken = db::get_setting(conn, K_STOKEN).unwrap_or_default();
    // xpan 的 uinfo 认 cookie 会话（网页的 gettemplatevariable 会返回 -6）
    let url = format!(
        "{WEB_BASE}/rest/2.0/xpan/nas?method=uinfo&clienttype=0&app_id=250528&web=1&bdstoken={}&t={}",
        q(&s.stoken),
        now_ms()
    );
    let v = web_get(&s, &url)?;
    if let Some(e) = api_error(&v) {
        return Err(e);
    }
    let user = v
        .get("netdisk_name")
        .and_then(|x| x.as_str())
        .filter(|x| !x.is_empty())
        .or_else(|| v.get("baidu_name").and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string();
    if !user.is_empty() {
        db::set_setting(conn, K_USER, &user);
    }
    let _ = credentials::set(C_STOKEN, &s.stoken);
    Ok(user)
}

/// 登录窗口用：直接拿 webview 里的 cookie 完成落盘
pub fn login_from_cookies(conn: &Connection, bduss: &str) -> Result<String, String> {
    save_login(conn, bduss, "", "")?;
    match sync_profile(conn) {
        // 换不到 bdstoken 不算致命：列表接口多数情况仍可用，播放再兜底
        Ok(user) => Ok(user),
        Err(_) => Ok(String::new()),
    }
}

// ---------- HTTP ----------

fn web_get(s: &Session, url: &str) -> Result<serde_json::Value, String> {
    let a = agent();
    let cookie = cookie_header(s);
    let resp = crate::net::with_retry(|| {
        a.get(url)
            .set("Cookie", &cookie)
            .set("Referer", &format!("{WEB_BASE}/disk/main"))
            .set("Accept", "application/json, text/plain, */*")
            .call()
    })?;
    resp.into_json::<serde_json::Value>()
        .map_err(|e| format!("百度网盘返回内容无法解析：{e}"))
}

/// 网页接口错误形状：`{"errno":-9,"request_id":...}`；-6 / -9 / 2 表示会话失效
fn api_error(v: &serde_json::Value) -> Option<String> {
    let errno = v.get("errno").and_then(|x| x.as_i64()).unwrap_or(0);
    if errno == 0 {
        return None;
    }
    if errno == -6 || errno == -9 || errno == 2 {
        return Some(SESSION_EXPIRED.to_string());
    }
    let msg = v
        .get("error_msg")
        .or_else(|| v.get("show_msg"))
        .or_else(|| v.get("request_id"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    Some(format!("百度网盘接口错误（errno {errno}）{msg}"))
}

/// 带会话调用；会话失效统一换成可读提示
pub fn with_session<T>(
    state: &AppState,
    f: impl Fn(&Session) -> Result<T, String>,
) -> Result<T, String> {
    let s = {
        let conn = state.db.lock();
        let mut s = session().ok_or("请先扫码登录百度网盘")?;
        s.stoken = db::get_setting(&conn, K_STOKEN).unwrap_or_default();
        s
    };
    match f(&s) {
        Err(e) if e == SESSION_EXPIRED => Err("百度网盘登录已失效，请重新扫码登录".into()),
        r => r,
    }
}

// ---------- 目录浏览 ----------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BdItem {
    pub fs_id: String,
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
    pub ext: String,
}

fn item_from(v: &serde_json::Value) -> Option<BdItem> {
    let isdir = v.get("isdir").and_then(|x| x.as_i64()).unwrap_or(0) == 1;
    let path = v
        .get("path")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let name = v
        .get("server_filename")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.rsplit('/').next().unwrap_or("").to_string());
    let ext = if isdir {
        String::new()
    } else {
        name.rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default()
    };
    let fs_id = v
        .get("fs_id")
        .and_then(|x| x.as_i64())
        .map(|n| n.to_string())
        .or_else(|| v.get("fs_id").and_then(|x| x.as_str()).map(|s| s.to_string()))?;
    Some(BdItem {
        fs_id,
        name,
        path,
        is_dir: isdir,
        size: v.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
        mtime: v.get("server_mtime").and_then(|x| x.as_i64()).unwrap_or(0),
        ext,
    })
}

fn norm_dir(dir: &str) -> String {
    let d = dir.trim();
    if d.is_empty() || d == "/" {
        return "/".to_string();
    }
    format!("/{}", d.trim_matches('/'))
}

/// 网页版列目录（分页 1000 一页）
fn list_page(s: &Session, dir: &str, page: u64) -> Result<Vec<BdItem>, String> {
    let url = format!(
        "{WEB_LIST}?order=name&desc=0&showempty=0&web=1&page={page}&num=1000&dir={}&t={}&channel=chunlei&app_id=250528&bdstoken={}&clienttype=0&logid={}",
        q(&norm_dir(dir)),
        now_ms(),
        q(&s.stoken),
        now_ms()
    );
    let v = web_get(s, &url)?;
    if let Some(e) = api_error(&v) {
        return Err(e);
    }
    Ok(v
        .get("list")
        .and_then(|x| x.as_array())
        .map(|arr| arr.iter().filter_map(item_from).collect())
        .unwrap_or_default())
}

fn list_all_pages(s: &Session, dir: &str) -> Result<Vec<BdItem>, String> {
    let mut out = Vec::new();
    let mut page = 1u64;
    loop {
        let batch = list_page(s, dir, page)?;
        let n = batch.len();
        out.extend(batch);
        if n < 1000 || page >= 100 {
            break;
        }
        page += 1;
    }
    Ok(out)
}

/// 列一层目录（文件夹 + 可播放音频）
pub fn list_dir(s: &Session, dir: &str) -> Result<Vec<BdItem>, String> {
    let mut items = list_all_pages(s, dir)?;
    items.retain(|i| i.is_dir || is_audio(&i.ext));
    items.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Ok(items)
}

/// 递归列出 root 下所有音频（网页接口没有 recursion 参数，逐层广度遍历）
pub fn list_all(s: &Session, root: &str) -> Result<Vec<BdItem>, String> {
    let mut out = Vec::new();
    let mut queue: Vec<String> = vec![norm_dir(root)];
    let mut visited = 0usize;
    while let Some(dir) = queue.pop() {
        let batch = list_all_pages(s, &dir)?;
        visited += 1;
        for it in batch {
            if it.is_dir {
                // 防御：异常深/异常宽的目录树不要无限展开
                if visited < 4000 {
                    queue.push(it.path.clone());
                }
            } else if is_audio(&it.ext) {
                out.push(it);
            }
        }
        if visited > 4000 {
            break;
        }
    }
    Ok(out)
}

fn is_audio(ext: &str) -> bool {
    AUDIO_EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

// ---------- 导入与专辑索引 ----------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BdTrack {
    pub fs_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub ext: String,
    pub size: u64,
    pub path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BdAlbum {
    pub album: String,
    pub artist: String,
    pub count: u32,
    pub size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportStat {
    pub root: String,
    pub scanned: u32,
    pub imported: u32,
    pub skipped: u32,
    /// 导入后自动建/更新的播放列表名（= 导入文件夹名）
    pub playlist: String,
    pub playlist_id: i64,
    /// 本次新加入播放列表的曲目数（重复导入为 0）
    pub added: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootInfo {
    pub root: String,
    pub count: u32,
    pub size: u64,
}

/// 去掉文件名前缀的曲号："01. "、"03 - "、"12、"（纯年份 "1979" 不会被吃掉）
fn strip_track_no(stem: &str) -> String {
    let s = stem.trim();
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 2 {
        return s.to_string();
    }
    let rest = &s[digits.len()..];
    let rest = rest.trim_start();
    if let Some(r) = rest
        .strip_prefix('.')
        .or_else(|| rest.strip_prefix('、'))
        .or_else(|| rest.strip_prefix(')'))
    {
        return r.trim_start().to_string();
    }
    if let Some(r) = rest.strip_prefix('-') {
        let r = r.trim_start();
        if !r.is_empty() {
            return r.to_string();
        }
    }
    s.to_string()
}

/// 从「根目录 + 相对路径」推导专辑 / 歌手 / 歌名：
/// `/V家专辑/专辑名/01. 歌手 - 歌名.flac` → artist=V家专辑(可被文件名覆盖), album=专辑名
fn meta_from(root: &str, item: &BdItem) -> (String, String, String) {
    let rel = item
        .path
        .strip_prefix(&norm_dir(root))
        .unwrap_or(&item.path)
        .trim_start_matches('/');
    let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    let album = if segs.len() >= 2 {
        segs[segs.len() - 2].to_string()
    } else {
        norm_dir(root)
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or("网盘音乐")
            .to_string()
    };
    let dir_artist = if segs.len() >= 3 {
        segs[segs.len() - 3].to_string()
    } else {
        String::new()
    };
    let stem = item
        .name
        .rsplit_once('.')
        .map(|(a, _)| a)
        .unwrap_or(&item.name)
        .to_string();
    let cleaned = strip_track_no(&stem);
    let (mut artist, mut title) = (dir_artist, cleaned.clone());
    for sep in [" - ", " – ", "—"] {
        if let Some((a, t)) = cleaned.split_once(sep) {
            let (a, t) = (a.trim(), t.trim());
            if !a.is_empty() && !t.is_empty() {
                artist = a.to_string();
                title = t.to_string();
            }
            break;
        }
    }
    if title.is_empty() {
        title = stem;
    }
    (album, artist, title)
}

fn roots_list(conn: &Connection) -> Vec<String> {
    db::get_setting(conn, K_ROOT)
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
}

fn set_roots(conn: &Connection, roots: &[String]) {
    db::set_setting(
        conn,
        K_ROOT,
        &serde_json::to_string(roots).unwrap_or_else(|_| "[]".into()),
    );
}

pub fn roots(conn: &Connection) -> Vec<RootInfo> {
    roots_list(conn)
        .into_iter()
        .map(|root| {
            let (count, size) = conn
                .query_row(
                    "SELECT COUNT(*), COALESCE(SUM(size),0) FROM baidu_tracks WHERE root = ?1",
                    params![root],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
                )
                .unwrap_or((0, 0));
            RootInfo {
                root,
                count: count as u32,
                size: size.max(0) as u64,
            }
        })
        .collect()
}

/// 导入（或刷新）一个网盘文件夹：递归扫描音频并按目录结构生成专辑
pub fn import_root(state: &AppState, root: &str) -> Result<ImportStat, String> {
    let root = norm_dir(root);
    let mut items = with_session(state, |s| list_all(s, &root))?;
    // 播放列表按「专辑 → 路径」排好，多专辑文件夹导入后是一张有序的整单
    items.sort_by(|a, b| {
        let (aa, _, _) = meta_from(&root, a);
        let (ba, _, _) = meta_from(&root, b);
        aa.cmp(&ba).then_with(|| a.path.cmp(&b.path))
    });
    let scanned = items.len() as u32;
    let mut conn = state.db.lock();
    let ts = now_ms() as i64 / 1000;
    let mut imported = 0u32;
    {
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for it in &items {
            let (album, artist, title) = meta_from(&root, it);
            tx.execute(
                "INSERT INTO baidu_tracks(fs_id, root, path, album, artist, title, ext, size, mtime, imported_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT(fs_id) DO UPDATE SET
                   root=excluded.root, path=excluded.path, album=excluded.album,
                   artist=excluded.artist, title=excluded.title, ext=excluded.ext,
                   size=excluded.size, mtime=excluded.mtime, imported_at=excluded.imported_at",
                params![
                    it.fs_id,
                    root,
                    it.path,
                    album,
                    artist,
                    title,
                    it.ext,
                    it.size as i64,
                    it.mtime,
                    ts
                ],
            )
            .map_err(|e| format!("写入导入记录失败：{e}"))?;
            imported += 1;
        }
        let mut all = roots_list(&tx);
        if !all.iter().any(|r| r == &root) {
            all.push(root.clone());
            set_roots(&tx, &all);
        }
        tx.commit().map_err(|e| e.to_string())?;
    }

    // 导入即加入播放列表（与网易云/QQ/酷狗歌单导入走同一套入库逻辑）：
    // 列表名取导入文件夹名，remote_id = 网盘目录路径，重复导入合并去重
    let name = root_name(&root);
    let rows: Vec<crate::commands::playlists::OnlineTrackRow> = items
        .iter()
        .map(|it| {
            let (album, artist, title) = meta_from(&root, it);
            crate::commands::playlists::OnlineTrackRow {
                rid: it.fs_id.clone(),
                title,
                artist,
                album,
                cover: String::new(),
                duration_ms: 0,
                media_mid: it.ext.clone(),
                vip: false,
            }
        })
        .collect();
    let (playlist_id, added) =
        crate::commands::playlists::import_online_tracks(&conn, KIND, &root, &name, &rows)?;

    Ok(ImportStat {
        root,
        scanned,
        imported,
        skipped: scanned.saturating_sub(imported),
        playlist: name,
        playlist_id,
        added: added as u32,
    })
}

/// 目录按 path 切出最后一段作为播放列表名
fn root_name(root: &str) -> String {
    root.trim_end_matches('/')
        .rsplit('/')
        .find(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "百度网盘".to_string())
}

pub fn remove_root(state: &AppState, root: &str) -> Result<u32, String> {
    let root = norm_dir(root);
    let mut conn = state.db.lock();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    if let Some(pid) = crate::db::find_playlist_by_remote(&tx, KIND, &root, "") {
        crate::db::delete_playlist(&tx, pid);
    }
    let _ = tx.execute(
        "DELETE FROM online_tracks WHERE kind = ?1
           AND rid IN (SELECT fs_id FROM baidu_tracks WHERE root = ?2)",
        params![KIND, root],
    );
    let n = tx
        .execute("DELETE FROM baidu_tracks WHERE root = ?1", params![root])
        .map_err(|e| e.to_string())?;
    let mut all = roots_list(&tx);
    all.retain(|r| r != &root);
    set_roots(&tx, &all);
    tx.commit().map_err(|e| e.to_string())?;
    Ok(n as u32)
}

/// 把库里已导入的曲目按导入目录重建播放列表（幂等）。
/// 0.7.2 之前的导入没有登记播放列表，升级后靠它一次性补齐；
/// 之后每次导入也会走同一套 `import_online_tracks`。
pub fn rebuild_playlists(conn: &Connection) -> Result<u32, String> {
    let roots = roots_list(conn);
    let mut n = 0u32;
    for root in roots {
        let mut stmt = conn
            .prepare(
                "SELECT fs_id, title, artist, album, ext FROM baidu_tracks
                 WHERE root = ?1 ORDER BY album, path",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<crate::commands::playlists::OnlineTrackRow> = stmt
            .query_map(params![root], |r| {
                Ok(crate::commands::playlists::OnlineTrackRow {
                    rid: r.get(0)?,
                    title: r.get(1)?,
                    artist: r.get(2)?,
                    album: r.get(3)?,
                    cover: String::new(),
                    duration_ms: 0,
                    media_mid: r.get::<_, String>(4)?,
                    vip: false,
                })
            })
            .map_err(|e| e.to_string())?
            .filter_map(|x| x.ok())
            .collect();
        if rows.is_empty() {
            continue;
        }
        let name = root_name(&root);
        let (_pid, _added) =
            crate::commands::playlists::import_online_tracks(conn, KIND, &root, &name, &rows)?;
        n += 1;
    }
    Ok(n)
}

pub fn albums(conn: &Connection) -> Vec<BdAlbum> {
    let mut stmt = match conn.prepare(
        "SELECT album, MIN(artist), COUNT(*), COALESCE(SUM(size),0)
         FROM baidu_tracks GROUP BY album ORDER BY MIN(artist), album",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = stmt.query_map([], |r| {
        Ok(BdAlbum {
            album: r.get::<_, String>(0)?,
            artist: r.get::<_, String>(1).unwrap_or_default(),
            count: r.get::<_, i64>(2).unwrap_or(0) as u32,
            size: r.get::<_, i64>(3).unwrap_or(0).max(0) as u64,
        })
    });
    match rows {
        Ok(it) => it.flatten().collect(),
        Err(_) => Vec::new(),
    }
}

fn track_rows(conn: &Connection, sql: &str, p: &[&str]) -> Vec<BdTrack> {
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = stmt.query_map(rusqlite::params_from_iter(p.iter().copied()), |r| {
        Ok(BdTrack {
            fs_id: r.get::<_, String>(0)?,
            title: r.get::<_, String>(1)?,
            artist: r.get::<_, String>(2)?,
            album: r.get::<_, String>(3)?,
            ext: r.get::<_, String>(4)?,
            size: r.get::<_, i64>(5).unwrap_or(0).max(0) as u64,
            path: r.get::<_, String>(6)?,
        })
    });
    match rows {
        Ok(it) => it.flatten().collect(),
        Err(_) => Vec::new(),
    }
}

pub fn album_tracks(conn: &Connection, album: &str) -> Vec<BdTrack> {
    track_rows(
        conn,
        "SELECT fs_id, title, artist, album, ext, size, path FROM baidu_tracks
         WHERE album = ?1 ORDER BY title",
        &[album],
    )
}

pub fn all_tracks(conn: &Connection) -> Vec<BdTrack> {
    track_rows(
        conn,
        "SELECT fs_id, title, artist, album, ext, size, path FROM baidu_tracks
         ORDER BY album, title",
        &[],
    )
}

// ---------- 播放 ----------

/// 取直链：dlink 是限时签名地址（数小时有效），每次播放现取。
/// 依次尝试：xpan multimedia（fs_id）→ 网页 filemetas（path）。
/// 下载 dlink 时要带的 Cookie（BDUSS + STOKEN），否则百度 403
pub fn download_cookie() -> Option<String> {
    let bduss = credentials::get(C_BDUSS).unwrap_or_default();
    if bduss.trim().is_empty() {
        return None;
    }
    let stoken = credentials::get(C_STOKEN).unwrap_or_default();
    Some(format!("BDUSS={}; STOKEN={}", bduss.trim(), stoken))
}

pub fn dlink(s: &Session, fs_id: &str, path: &str) -> Result<String, String> {
    // 1) 网页 filemetas：按路径取（认 cookie，实测可用）
    if !path.is_empty() {
        let url = format!(
            "{WEB_FILEMETAS}?target={}&dlink=1&web=5&app_id=250528&bdstoken={}&channel=chunlei&clienttype=0&t={}",
            q(&format!("[\"{}\"]", path.replace('"', "\\\""))),
            q(&s.stoken),
            now_ms()
        );
        match web_get(s, &url) {
            Ok(v) => {
                if std::env::var_os("RM_BAIDU_DEBUG").is_some() {
                    eprintln!(
                        "[baidu] web filemetas = {}",
                        v.to_string().chars().take(200).collect::<String>()
                    );
                }
                if let Some(e) = api_error(&v) {
                    return Err(e);
                }
                if let Some(d) = first_dlink(&v) {
                    return Ok(d);
                }
            }
            Err(e) => {
                if std::env::var_os("RM_BAIDU_DEBUG").is_some() {
                    eprintln!("[baidu] web filemetas err = {e}");
                }
            }
        }
    }
    // 2) xpan multimedia（需要开放平台授权，普通 cookie 会话一般返回 9019）
    if !fs_id.is_empty() {
        let url = format!(
            "{XPAN_MULTIMEDIA}?method=filemetas&fsids=[{}]&dlink=1&thumb=0&extra=0&bdstoken={}&app_id=250528&web=1&clienttype=0&channel=chunlei&t={}",
            q(fs_id),
            q(&s.stoken),
            now_ms()
        );
        match web_get(s, &url) {
            Ok(v) => {
                if std::env::var_os("RM_BAIDU_DEBUG").is_some() {
                    eprintln!(
                        "[baidu] xpan filemetas = {}",
                        v.to_string().chars().take(200).collect::<String>()
                    );
                }
                if let Some(d) = first_dlink(&v) {
                    return Ok(d);
                }
            }
            Err(e) => {
                if std::env::var_os("RM_BAIDU_DEBUG").is_some() {
                    eprintln!("[baidu] xpan filemetas err = {e}");
                }
            }
        }
    }
    Err("未取到下载直链（可能是会员专属文件或权限不足）".into())
}

fn first_dlink(v: &serde_json::Value) -> Option<String> {
    let arr = v
        .get("list")
        .or_else(|| v.get("info"))
        .and_then(|x| x.as_array())?;
    let d = arr
        .first()?
        .get("dlink")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    if d.is_empty() {
        None
    } else {
        Some(d.to_string())
    }
}

// ---------- 扫码登录（百度 passport 的二维码通道） ----------
//
// 流程（与网页版一致）：
//   1. getqrcode → sign（同时就是轮询用的 channel_id）+ 二维码图片
//   2. channel/unicast 长轮询：未扫码时挂住（读超时即视为"等待中"），
//      扫码后返回 channel_v = {"status":0}（待确认）或 {"v":"…","status":1}（已确认）
//   3. bdussign?v=… → 拿到 BDUSS / STOKEN，等同于网页登录后的 cookie
//
// 之所以不走「应用内开 WebView 打开登录页」：passport 的登录框依赖一串 CDN 脚本，
// 在 WKWebView 里经常只渲染出背景插画（实测），而二维码接口是稳定的明文协议。

const PASSPORT: &str = "https://passport.baidu.com";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QrTicket {
    pub sign: String,
    /// data:image/png;base64,… 直接给 <img src>
    pub image: String,
    pub prompt: String,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QrState {
    /// waiting | scanned | done | expired
    pub state: String,
    pub user: String,
}

/* ── cookie jar（passport 的登录态要靠一串 cookie 串起来） ────────── */

fn jar() -> &'static std::sync::Mutex<std::collections::HashMap<String, String>> {
    static JAR: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, String>>> =
        std::sync::OnceLock::new();
    JAR.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn jar_cookie_header() -> String {
    let j = jar().lock().unwrap_or_else(|e| e.into_inner());
    j.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn jar_absorb(resp: &ureq::Response) {
    let mut j = jar().lock().unwrap_or_else(|e| e.into_inner());
    for raw in resp.all("set-cookie") {
        let first = raw.split(';').next().unwrap_or("");
        if let Some((k, v)) = first.split_once('=') {
            let k = k.trim();
            let v = v.trim();
            if !k.is_empty() {
                if v.is_empty() {
                    j.remove(k);
                } else {
                    j.insert(k.to_string(), v.to_string());
                }
            }
        }
    }
}

fn passport_agent(read_secs: u64) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(8))
        .timeout_read(Duration::from_secs(read_secs))
        .user_agent(WEB_UA)
        .build()
}

fn passport_get(a: &ureq::Agent, url: &str) -> Result<ureq::Response, ureq::Error> {
    let cookie = jar_cookie_header();
    let mut req = a
        .get(url)
        .set("Referer", &format!("{PASSPORT}/"))
        .set("Accept", "application/json, text/plain, */*");
    if !cookie.is_empty() {
        req = req.set("Cookie", &cookie);
    }
    // 4xx/5xx 也把响应体交回去：百度的报错信息在 body 里，比状态码有用
    let resp = match req.call() {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(e),
    };
    jar_absorb(&resp);
    Ok(resp)
}

/// 百度这类接口偶尔会包一层 `callback({...})`，统一剥掉再解析
fn strip_jsonp(text: &str) -> &str {
    let t = text.trim();
    if let (Some(open), Some(_close)) = (t.find('('), t.rfind(')')) {
        if open > 0 && !t.starts_with('{') && !t.starts_with('[') {
            return t[open + 1..t.rfind(')').unwrap_or(t.len())].trim();
        }
    }
    t
}

pub fn qr_begin() -> Result<QrTicket, String> {
    let a = passport_agent(20);
    let t = now_ms();
    let url = format!("{PASSPORT}/v2/api/getqrcode?lp=pc&qrloginfrom=pc&t={t}");
    let resp = crate::net::with_retry(|| passport_get(&a, &url))?;
    let text = resp
        .into_string()
        .map_err(|e| format!("获取二维码失败：{e}"))?;
    let v: serde_json::Value = serde_json::from_str(strip_jsonp(&text))
        .map_err(|_| format!("获取二维码失败：返回内容异常（{}）", text.chars().take(120).collect::<String>()))?;
    let errno = v.get("errno").and_then(|x| x.as_i64()).unwrap_or(-1);
    if errno != 0 {
        return Err(format!("获取二维码失败（errno {errno}）"));
    }
    let sign = v
        .get("sign")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if sign.is_empty() {
        return Err("百度没有返回二维码标识，请重试".into());
    }
    let prompt = v
        .get("prompt")
        .and_then(|x| x.as_str())
        .unwrap_or("请用手机百度 App 扫码登录")
        .to_string();

    // 二维码图片（245×245 PNG）
    let img_url = format!("{PASSPORT}/v2/api/qrcode?sign={sign}&lp=pc&qrloginfrom=pc");
    let resp = crate::net::with_retry(|| passport_get(&a, &img_url))?;
    let mut buf: Vec<u8> = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .map_err(|e| format!("读取二维码图片失败：{e}"))?;
    if buf.len() < 64 {
        return Err("二维码图片为空，请重试".into());
    }
    use base64::Engine as _;
    let image = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&buf)
    );
    Ok(QrTicket {
        sign,
        image,
        prompt,
    })
}

/// 轮询扫码状态。`state` 取值：waiting / scanned / done / expired
pub fn qr_poll(state: &AppState, sign: &str) -> Result<QrState, String> {
    if sign.trim().is_empty() {
        return Err("二维码已失效，请刷新".into());
    }
    let a = passport_agent(6);
    let url = format!(
        "{PASSPORT}/channel/unicast?channel_id={}&t={}&callback=",
        sign,
        now_ms()
    );
    let body = match passport_get(&a, &url) {
        Ok(resp) => resp.into_string().unwrap_or_default(),
        // 长轮询读超时 / 网络抖动 = 还没有人扫码
        Err(_) => {
            return Ok(QrState {
                state: "waiting".into(),
                user: String::new(),
            })
        }
    };
    let body = strip_jsonp(&body).trim();
    if body.is_empty() {
        return Ok(QrState {
            state: "waiting".into(),
            user: String::new(),
        });
    }
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            return Ok(QrState {
                state: "waiting".into(),
                user: String::new(),
            })
        }
    };
    if std::env::var_os("RM_BAIDU_DEBUG").is_some() {
        eprintln!("[baidu] poll body = {body}");
    }
    // 百度偶尔对同一 channel 的并发查询返回非 0：当成"还没扫码"继续等，
    // 真正的过期由前端按时间判断，避免误杀还在等确认的二维码。
    let errno = v.get("errno").and_then(|x| x.as_i64()).unwrap_or(0);
    if errno != 0 {
        return Ok(QrState {
            state: "waiting".into(),
            user: String::new(),
        });
    }
    let channel_v = v.get("channel_v").and_then(|x| x.as_str()).unwrap_or("");
    if channel_v.is_empty() {
        return Ok(QrState {
            state: "waiting".into(),
            user: String::new(),
        });
    }
    let inner: serde_json::Value =
        serde_json::from_str(channel_v).unwrap_or(serde_json::Value::Null);
    let status = inner.get("status").and_then(|x| x.as_i64()).unwrap_or(-1);
    let token = inner.get("v").and_then(|x| x.as_str()).unwrap_or("");
    if status == 0 && token.is_empty() {
        // 已扫码，等手机上点「确认登录」
        return Ok(QrState {
            state: "scanned".into(),
            user: String::new(),
        });
    }
    if token.is_empty() {
        return Ok(QrState {
            state: "waiting".into(),
            user: String::new(),
        });
    }

    // 确认后换 BDUSS：登录页真实走的是 /v3/login/main/qrbdusslogin
    // （channel_v.v 本身是临时 bduss，这个接口把它换成正式登录态并回写 cookie）
    let token = token_value(token);
    let url = format!(
        "{PASSPORT}/v3/login/main/qrbdusslogin?v={}&bduss={}&u=&loginVersion=v4&qrcode=1&tpl=netdisk&callback=cb",
        now_ms(),
        q(&token)
    );
    let resp = passport_get(&a, &url).map_err(|e| format!("换取登录态失败：{e}"))?;
    let status = resp.status();
    let cookies = resp
        .all("set-cookie")
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>();
    let text = resp.into_string().unwrap_or_default();
    if std::env::var_os("RM_BAIDU_DEBUG").is_some() {
        // 只打状态与长度，避免把 bduss/stoken 写进日志
        eprintln!(
            "[baidu] qrbdusslogin status = {status}, body_len = {}",
            text.len()
        );
    }
    let mut bduss = String::new();
    let mut stoken = String::new();
    let mut user = String::new();
    let parsed = strip_jsonp(&text);
    if let Ok(j) = serde_json::from_str::<serde_json::Value>(parsed) {
        // 正常返回：{"errInfo":{"no":0,...},"data":{"session":{"bduss":…,"stoken":…},"user":{"username":…}}}
        if let Some(d) = j.get("data") {
            if let Some(sess) = d.get("session") {
                bduss = sess.get("bduss").and_then(|x| x.as_str()).unwrap_or("").to_string();
                stoken = sess.get("stoken").and_then(|x| x.as_str()).unwrap_or("").to_string();
            }
            if let Some(u) = d.get("user") {
                user = u.get("username").and_then(|x| x.as_str()).unwrap_or("").to_string();
                if user.is_empty() {
                    user = u.get("displayName").and_then(|x| x.as_str()).unwrap_or("").to_string();
                }
            }
        }
        // 兜底：顶层的 bduss/stoken
        if bduss.is_empty() {
            bduss = j.get("bduss").and_then(|x| x.as_str()).unwrap_or("").to_string();
        }
        if stoken.is_empty() {
            stoken = j.get("stoken").and_then(|x| x.as_str()).unwrap_or("").to_string();
        }
    }
    for raw in &cookies {
        let first = raw.split(';').next().unwrap_or("");
        if let Some((k, val)) = first.split_once('=') {
            if k.trim().eq_ignore_ascii_case("BDUSS") && bduss.is_empty() {
                bduss = val.trim().to_string();
            }
            if k.trim().eq_ignore_ascii_case("STOKEN") && stoken.is_empty() {
                stoken = val.trim().to_string();
            }
        }
    }
    if bduss.is_empty() {
        let snippet: String = text.chars().take(160).collect();
        return Err(format!(
            "登录确认后没拿到百度登录态（HTTP {status}，响应：{}）",
            snippet.replace('\n', " ")
        ));
    }

    let user = {
        let conn = state.db.lock();
        save_login(&conn, &bduss, &stoken, "")?;
        sync_profile(&conn).unwrap_or_default()
    };
    Ok(QrState {
        state: "done".into(),
        user,
    })
}

/// 去掉 token 里可能带的 JSON 引号/转义
fn token_value(raw: &str) -> String {
    raw.trim_matches('"').replace("\\/", "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str) -> BdItem {
        BdItem {
            fs_id: "1".into(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            path: path.to_string(),
            is_dir: false,
            size: 0,
            mtime: 0,
            ext: path.rsplit('.').next().unwrap_or("").to_lowercase(),
        }
    }

    #[test]
    fn 只认音频扩展名_视频容器不算() {
        for ext in ["mp3", "FLAC", "wav", "ogg", "oga", "m4a", "aac"] {
            assert!(is_audio(ext), "{ext} 应该算音频");
        }
        // mp4 / m4b 是视频、有声书容器，导入时要跳过
        for ext in ["mp4", "m4b", "ape", "wma", "jpg", "txt", "zip", ""] {
            assert!(!is_audio(ext), "{ext} 不该算音频");
        }
    }

    #[test]
    fn 播放列表名取最后一段路径() {
        assert_eq!(root_name("/我的资源/音乐songs/V家专辑"), "V家专辑");
        assert_eq!(root_name("/我的资源/音乐songs/V家专辑/"), "V家专辑");
        assert_eq!(root_name("/"), "百度网盘");
    }

    #[test]
    fn 专辑歌手从目录结构与文件名推导() {
        let root = "/我的资源/音乐songs/V家专辑";
        let (album, artist, title) =
            meta_from(root, &item("/我的资源/音乐songs/V家专辑/【纯白2021】/01. 洛天依 - 千本樱.flac"));
        assert_eq!(album, "【纯白2021】");
        assert_eq!(artist, "洛天依");
        assert_eq!(title, "千本樱");

        // 没有「歌手 - 歌名」、也没有歌手层目录 → 歌手留空（界面显示「未知歌手」）
        let (album, artist, title) =
            meta_from(root, &item("/我的资源/音乐songs/V家专辑/某专辑/02. 曲目2.wav"));
        assert_eq!(album, "某专辑");
        assert_eq!(artist, "");
        assert_eq!(title, "曲目2");

        // 目录里有歌手层时用倒数第三段
        let (album, artist, _title) =
            meta_from(root, &item("/我的资源/音乐songs/V家专辑/洛天依/某专辑/03 曲目3.wav"));
        assert_eq!(album, "某专辑");
        assert_eq!(artist, "洛天依");
    }
}
