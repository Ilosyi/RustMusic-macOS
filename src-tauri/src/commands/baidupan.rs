//! 百度网盘音源的命令层（扫码登录、浏览、导入、播放）。
//!
//! 与其它在线音源一致：网络与凭据 IO 全部丢进阻塞线程池（`super::blocking`）。
//! 阻塞任务不借用 `State`（闭包要求 'static），统一在任务体内用 `AppHandle`
//! 重新取 `AppState`——与 `navidrome_play_task` 同一写法。
//!
//! 登录走百度 passport 的二维码通道（见 `crate::baidupan` 里的 qr_begin / qr_poll），
//! 不需要用户注册开放平台应用，也不需要内嵌浏览器；前端只负责显示二维码并轮询。

use tauri::{AppHandle, Manager, State};

use crate::engine::TrackInfo;
use crate::AppState;

use super::blocking;
use super::engine_clone;


#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BaiduStatus {
    pub configured: bool,
    pub authorized: bool,
    pub user: String,
}

fn status_of(state: &AppState) -> BaiduStatus {
    let user = {
        let conn = state.db.lock();
        crate::baidupan::user_name(&conn)
    };
    BaiduStatus {
        configured: true,
        authorized: crate::baidupan::authorized(),
        user,
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BdPlayReq {
    pub fs_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub ext: String,
    #[serde(default)]
    pub duration_ms: u64,
}

#[tauri::command]
pub async fn baidu_status(app: AppHandle) -> Result<BaiduStatus, String> {
    blocking(move || {
        let state = app.state::<AppState>();
        let st = state.inner();
        let mut user = {
            let conn = st.db.lock();
            crate::baidupan::user_name(&conn)
        };
        // 已登录但还没存昵称（老会话）：顺手补一次，失败不影响状态
        if user.is_empty() && crate::baidupan::authorized() {
            let conn = st.db.lock();
            user = crate::baidupan::sync_profile(&conn).unwrap_or_default();
        }
        Ok(BaiduStatus {
            configured: true,
            authorized: crate::baidupan::authorized(),
            user,
        })
    })
    .await
}

/// 取一张新的登录二维码（data:image/png;base64,…）
#[tauri::command]
pub async fn baidu_qr_begin() -> Result<crate::baidupan::QrTicket, String> {
    blocking(crate::baidupan::qr_begin).await
}

/// 轮询扫码状态：waiting / scanned / done / expired
#[tauri::command]
pub async fn baidu_qr_poll(app: AppHandle, sign: String) -> Result<crate::baidupan::QrState, String> {
    blocking(move || {
        let st = app.state::<AppState>();
        crate::baidupan::qr_poll(st.inner(), &sign)
    })
    .await
}

#[tauri::command]
pub async fn baidu_logout(state: State<'_, AppState>) -> Result<BaiduStatus, String> {
    {
        let conn = state.db.lock();
        crate::baidupan::logout(&conn);
    }
    Ok(status_of(&state))
}

#[tauri::command]
pub async fn baidu_list(
    app: AppHandle,
    dir: String,
) -> Result<Vec<crate::baidupan::BdItem>, String> {
    blocking(move || {
        let st = app.state::<AppState>();
        crate::baidupan::with_session(st.inner(), |s| crate::baidupan::list_dir(s, &dir))
    })
    .await
}

#[tauri::command]
pub async fn baidu_import(
    app: AppHandle,
    dir: String,
) -> Result<crate::baidupan::ImportStat, String> {
    blocking(move || {
        let st = app.state::<AppState>();
        crate::baidupan::import_root(st.inner(), &dir)
    })
    .await
}

#[tauri::command]
pub async fn baidu_roots(
    state: State<'_, AppState>,
) -> Result<Vec<crate::baidupan::RootInfo>, String> {
    let conn = state.db.lock();
    Ok(crate::baidupan::roots(&conn))
}

#[tauri::command]
pub async fn baidu_remove_root(app: AppHandle, root: String) -> Result<u32, String> {
    blocking(move || {
        let st = app.state::<AppState>();
        crate::baidupan::remove_root(st.inner(), &root)
    })
    .await
}

#[tauri::command]
pub async fn baidu_albums(
    state: State<'_, AppState>,
) -> Result<Vec<crate::baidupan::BdAlbum>, String> {
    let conn = state.db.lock();
    Ok(crate::baidupan::albums(&conn))
}

#[tauri::command]
pub async fn baidu_album_tracks(
    state: State<'_, AppState>,
    album: String,
) -> Result<Vec<crate::baidupan::BdTrack>, String> {
    let conn = state.db.lock();
    Ok(crate::baidupan::album_tracks(&conn, &album))
}

#[tauri::command]
pub async fn baidu_all_tracks(
    state: State<'_, AppState>,
) -> Result<Vec<crate::baidupan::BdTrack>, String> {
    let conn = state.db.lock();
    Ok(crate::baidupan::all_tracks(&conn))
}

/// 播放：现取 dlink（限时签名，约 8 小时）→ 交给引擎边下边播
#[tauri::command]
pub async fn baidu_play(app: AppHandle, track: BdPlayReq) -> Result<(), String> {
    blocking(move || baidu_play_task(&app, track)).await
}

fn baidu_play_task(app: &AppHandle, track: BdPlayReq) -> Result<(), String> {
    let state = app.state::<AppState>();
    let st = state.inner();
    // 直链优先用「路径」查（网页接口认 cookie）；导入过的曲子路径存在库里
    let path = {
        let conn = st.db.lock();
        crate::db::baidu_path_of(&conn, &track.fs_id).unwrap_or_default()
    };
    let url = crate::baidupan::with_session(st, |s| {
        crate::baidupan::dlink(s, &track.fs_id, &path)
    })?;
    let info = TrackInfo {
        id: None,
        kind: crate::baidupan::KIND.into(),
        path: String::new(),
        title: track.title,
        artist: track.artist,
        album: track.album,
        cover: String::new(),
        duration_ms: track.duration_ms,
        nid: None,
        qid: Some(track.fs_id),
        kgid: None,
        quality: if track.ext.is_empty() {
            None
        } else {
            Some(track.ext)
        },
    };
    super::record_online_play(&state, &info, "", false);
    engine_clone(&state).play_url(url, info)
}

/// 试听：浏览目录时直接听某个网盘文件（不写播放记录）
#[tauri::command]
pub async fn baidu_preview(app: AppHandle, fs_id: String, path: String) -> Result<(), String> {
    blocking(move || {
        let state = app.state::<AppState>();
        let st = state.inner();
        let url = crate::baidupan::with_session(st, |s| {
            crate::baidupan::dlink(s, &fs_id, &path)
        })?;
        let name = path.rsplit('/').next().unwrap_or("网盘音频").to_string();
        let title = name
            .rsplit_once('.')
            .map(|(a, _)| a.to_string())
            .unwrap_or(name);
        let info = TrackInfo {
            id: None,
            kind: crate::baidupan::KIND.into(),
            path: String::new(),
            title,
            artist: String::new(),
            album: String::new(),
            cover: String::new(),
            duration_ms: 0,
            nid: None,
            qid: Some(fs_id),
            kgid: None,
            quality: None,
        };
        engine_clone(&state).play_url(url, info)
    })
    .await
}
