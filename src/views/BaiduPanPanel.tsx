//! 百度网盘音源面板（三个界面风格共用）。
//!
//! 只用 `--shade/--line/--ink*` 这套 CSS 变量和 `btn-primary/btn-secondary`，
//! 因此标准 / 拟物 / 液态玻璃三套主题下都能直接复用同一个组件。
//!
//! 流程：点「扫码登录」→ 后端取一张百度登录二维码（passport 的 getqrcode 接口）
//! → 面板里直接显示二维码 → 手机百度 App 扫码并确认 → 前端轮询到 done
//! → 浏览网盘目录 →「导入该文件夹」把目录树里的音频收进本地库 → 按专辑 / 单曲播放。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Album as AlbumIcon,
  ArrowLeft,
  ChevronRight,
  Cloud,
  FolderClosed,
  FolderPlus,
  Loader2,
  LogOut,
  Music2,
  Play,
  QrCode,
  RefreshCw,
  ShieldCheck,
  Trash2,
  User,
} from "lucide-react";
import { api } from "../api";
import { useStore } from "../store";
import type {
  BdAlbum,
  BdItem,
  BdTrack,
  BaiduQrTicket,
  BaiduRootInfo,
  BaiduStatus,
} from "../types";

/** 与后端 `AUDIO_EXTS` 保持一致：能解码的才允许直接播 */
const AUDIO_EXT = new Set(["mp3", "flac", "wav", "ogg", "oga", "m4a", "aac", "mp4", "m4b"]);

function fmtSize(n: number): string {
  if (!n) return "—";
  if (n < 1024) return `${n}B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)}KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)}MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)}GB`;
}

/** 把网盘绝对路径拆成面包屑（"我的资源/音乐/V家专辑" → ["我的资源","音乐","V家专辑"]） */
function crumbs(dir: string): { name: string; path: string }[] {
  const parts = dir.split("/").filter(Boolean);
  const out: { name: string; path: string }[] = [{ name: "我的网盘", path: "/" }];
  let acc = "";
  for (const p of parts) {
    acc += `/${p}`;
    out.push({ name: p, path: acc });
  }
  return out;
}


export default function BaiduPanPanel() {
  const toast = useStore((s) => s.toast);
  const refreshPlaylists = useStore((s) => s.refreshPlaylists);

  const [status, setStatus] = useState<BaiduStatus | null>(null);
  const [busy, setBusy] = useState("");
  const [dir, setDir] = useState("");
  const [items, setItems] = useState<BdItem[]>([]);
  const [roots, setRoots] = useState<BaiduRootInfo[]>([]);
  const [albums, setAlbums] = useState<BdAlbum[]>([]);
  const [openAlbum, setOpenAlbum] = useState<{ album: string; tracks: BdTrack[] } | null>(null);
  const [playingId, setPlayingId] = useState<string | null>(null);

  const authorized = !!status?.authorized;
  const [qr, setQr] = useState<BaiduQrTicket | null>(null);
  /** 二维码轮询定时器 */
  const pollRef = useRef<number | null>(null);

  const loadStatus = useCallback(async () => {
    try {
      setStatus(await api.baiduStatus());
    } catch (e) {
      toast(String(e), "error");
    }
  }, [toast]);

  const loadRoots = useCallback(async () => {
    try {
      setRoots(await api.baiduRoots());
      setAlbums(await api.baiduAlbums());
      // 首次拉根目录时后端会把历史导入补登记成播放列表，这里顺带刷新左侧列表
      await refreshPlaylists();
    } catch (e) {
      toast(String(e), "error");
    }
  }, [toast, refreshPlaylists]);

  const loadDir = useCallback(
    async (target: string) => {
      setBusy("list");
      try {
        const list = await api.baiduList(target);
        setItems(list);
        setDir(target);
        setOpenAlbum(null);
      } catch (e) {
        toast(String(e), "error");
      } finally {
        setBusy("");
      }
    },
    [toast],
  );

  useEffect(() => {
    void loadStatus();
    void loadRoots();
  }, [loadStatus, loadRoots]);

  const stopPoll = useCallback(() => {
    if (pollRef.current !== null) {
      window.clearTimeout(pollRef.current);
      pollRef.current = null;
    }
  }, []);

  // 组件卸载时收掉定时器
  useEffect(() => stopPoll, [stopPoll]);

  // 登录态就绪时自动展开根目录
  useEffect(() => {
    if (authorized && !dir && !busy) void loadDir("/");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [authorized]);

  const { dirs, files } = useMemo(() => {
    const d: BdItem[] = [];
    const f: BdItem[] = [];
    for (const it of items) {
      if (it.isDir) d.push(it);
      else if (AUDIO_EXT.has(it.ext.toLowerCase())) f.push(it);
    }
    const byName = (a: BdItem, b: BdItem) =>
      a.name.localeCompare(b.name, "zh-Hans-CN", { numeric: true });
    return { dirs: d.sort(byName), files: f.sort(byName) };
  }, [items]);

  /** 取一张新二维码并开始轮询（串行：上一次返回后再排下一次，避免并发长轮询） */
  const refreshQr = useCallback(async () => {
    stopPoll();
    setBusy("qr");
    try {
      const t = await api.baiduQrBegin();
      setQr(t);
      setBusy(""); 
      const startedAt = Date.now();

      const tick = () => {
        pollRef.current = window.setTimeout(async () => {
          try {
            const st = await api.baiduQrPoll(t.sign);
            if (st.state === "scanned") {
              setBusy("scanned");
            } else if (st.state === "done") {
              stopPoll();
              setQr(null);
              setBusy("");
              toast(`已连接百度网盘${st.user ? `：${st.user}` : ""}`, "success");
              await loadStatus();
              await loadRoots();
              await loadDir("/");
              return;
            } else if (st.state === "expired") {
              stopPoll();
              setQr(null);
              setBusy("");
              toast("二维码已过期，请重新获取", "error");
              return;
            }
            // 二维码大约 2 分钟内有效
            if (Date.now() - startedAt > 115_000) {
              stopPoll();
              setQr(null);
              setBusy("");
              toast("二维码已过期，请重新获取", "error");
              return;
            }
            tick();
          } catch (e) {
            stopPoll();
            setQr(null);
            setBusy("");
            toast(String(e), "error");
          }
        }, 400);
      };
      tick();
    } catch (e) {
      setBusy("");
      toast(String(e), "error");
    }
  }, [loadDir, loadRoots, loadStatus, stopPoll, toast]);

  const cancelQr = () => {
    stopPoll();
    setQr(null);
    setBusy("");
  };

  const logout = async () => {
    try {
      const s = await api.baiduLogout();
      setStatus(s);
      setItems([]);
      setDir("");
      setAlbums([]);
      setRoots([]);
      setOpenAlbum(null);
      toast("已退出百度网盘", "success");
    } catch (e) {
      toast(String(e), "error");
    }
  };

  const importDir = async () => {
    if (!dir) return;
    setBusy("import");
    try {
      const st = await api.baiduImport(dir);
      await loadRoots();
      await refreshPlaylists();
      // 导入即进播放列表：结果里带上列表名与新增条数，用户能在左侧直接找到
      toast(
        `已导入 ${st.imported} 首到播放列表「${st.playlist}」` +
          (st.added < st.imported ? `（${st.imported - st.added} 首此前已在该列表）` : ""),
        "success",
      );
    } catch (e) {
      toast(String(e), "error");
    } finally {
      setBusy("");
    }
  };

  const removeRoot = async (root: string) => {
    try {
      const n = await api.baiduRemoveRoot(root);
      await loadRoots();
      await refreshPlaylists();
      setOpenAlbum(null);
      toast(`已移除 ${n} 首`, "success");
    } catch (e) {
      toast(String(e), "error");
    }
  };

  const play = async (t: {
    fsId: string;
    title: string;
    artist: string;
    album: string;
    ext: string;
  }) => {
    setPlayingId(t.fsId);
    try {
      await api.baiduPlay({ ...t, durationMs: 0 });
    } catch (e) {
      toast(String(e), "error");
    } finally {
      setPlayingId(null);
    }
  };

  const preview = async (f: BdItem) => {
    setPlayingId(f.fsId);
    try {
      await api.baiduPreview(f.fsId, f.path);
    } catch (e) {
      toast(String(e), "error");
    } finally {
      setPlayingId(null);
    }
  };

  const openOne = async (a: BdAlbum) => {
    setBusy("album");
    try {
      const tracks = await api.baiduAlbumTracks(a.album);
      setOpenAlbum({ album: a.album, tracks });
    } catch (e) {
      toast(String(e), "error");
    } finally {
      setBusy("");
    }
  };

  /* ── 未登录：扫码卡片 ─────────────────────────────────────── */
  if (!authorized) {
    return (
      <div className="h-full flex items-center justify-center py-6">
        <div
          className="w-full max-w-[560px] rounded-2xl p-6"
          style={{ background: "var(--shade)", border: "1px solid var(--line)" }}
        >
          <div className="flex items-center gap-2 mb-1.5">
            <Cloud size={16} className="text-[var(--accent-strong)]" />
            <span className="text-[14px] text-[var(--ink)] font-medium">
              用自己的百度网盘当音乐库
            </span>
          </div>

          {!qr ? (
            <>
              <p className="text-[12.5px] text-[var(--ink-2)] leading-relaxed mb-4">
                扫码登录后，可以把网盘里的任意文件夹（例如
                <code className="text-[var(--ink)]"> /我的资源/音乐/V家专辑/专辑名 </code>
                ）整棵树导入成本地专辑，之后按专辑播放、进队列、看歌词都和本地音乐一样。
              </p>
              <ol className="text-[12.5px] text-[var(--ink-2)] leading-relaxed mb-5 space-y-1.5">
                <li className="flex gap-2">
                  <span className="text-[var(--ink-3)] tabular-nums">1.</span>
                  点「扫码登录」，下面会直接显示二维码（不用跳浏览器）。
                </li>
                <li className="flex gap-2">
                  <span className="text-[var(--ink-3)] tabular-nums">2.</span>
                  用手机百度 App 扫一下，并在手机上确认登录。
                </li>
                <li className="flex gap-2">
                  <span className="text-[var(--ink-3)] tabular-nums">3.</span>
                  回到这里进入「我的资源 / 音乐 / …」，点「导入该文件夹」。
                </li>
              </ol>
              <div className="flex items-center gap-2">
                <button
                  className="btn-primary h-9 px-4 text-[12.5px] flex items-center gap-1.5"
                  disabled={busy === "qr"}
                  onClick={() => void refreshQr()}
                >
                  {busy === "qr" ? (
                    <Loader2 size={14} className="animate-spin" />
                  ) : (
                    <QrCode size={14} />
                  )}
                  扫码登录
                </button>
                <span className="flex items-center gap-1 text-[11.5px] text-[var(--ink-3)] ml-auto">
                  <ShieldCheck size={12} />
                  登录态只存在本机，不上传
                </span>
              </div>
            </>
          ) : (
            <div className="flex gap-5 items-center">
              <div
                className="shrink-0 rounded-xl p-2 bg-white"
                style={{ border: "1px solid var(--line)" }}
              >
                <img
                  src={qr.image}
                  alt="百度登录二维码"
                  width={150}
                  height={150}
                  style={{ display: "block", width: 150, height: 150 }}
                />
              </div>
              <div className="min-w-0">
                <div className="text-[12.5px] text-[var(--ink)] mb-1">
                  {busy === "scanned" ? "已扫码，请在手机上确认" : "请用手机百度 App 扫码"}
                </div>
                <div className="text-[11.5px] text-[var(--ink-3)] leading-relaxed mb-3">
                  打开百度 App → 右上角「+」→ 扫一扫。
                  <br />
                  登录态只保存在本机，可随时退出。
                </div>
                <div className="flex items-center gap-2">
                  <button
                    className="btn-secondary h-8 px-3 text-[12px] flex items-center gap-1.5"
                    onClick={() => void refreshQr()}
                    disabled={busy === "qr"}
                  >
                    <RefreshCw size={12} className={busy === "qr" ? "animate-spin" : ""} />
                    换个二维码
                  </button>
                  <button className="btn-secondary h-8 px-3 text-[12px]" onClick={cancelQr}>
                    取消
                  </button>
                </div>
              </div>
            </div>
          )}
        </div>
      </div>
    );
  }

  /* ── 已登录：目录浏览 + 已导入 ─────────────────────────────── */
  const trail = crumbs(dir);

  return (
    <div className="h-full flex flex-col min-h-0">
      {/* 工具条：面包屑 + 账号 + 操作 */}
      <div className="flex items-center gap-2 py-2 flex-wrap">
        <div className="flex items-center gap-1 min-w-0 flex-1 text-[12.5px] text-[var(--ink-2)]">
          {trail.map((c, i) => (
            <span key={c.path} className="flex items-center gap-1 min-w-0">
              {i > 0 && <ChevronRight size={12} className="text-[var(--ink-3)] shrink-0" />}
              <button
                className={`truncate hover:text-[var(--ink)] transition-colors ${
                  i === trail.length - 1 ? "text-[var(--ink)] font-medium" : ""
                }`}
                onClick={() => loadDir(c.path)}
              >
                {c.name}
              </button>
            </span>
          ))}
        </div>

        <button
          className="btn-secondary h-8 px-3 text-[12px] shrink-0 flex items-center gap-1.5"
          disabled={busy === "list"}
          onClick={() => loadDir(dir)}
          title="刷新当前目录"
        >
          <RefreshCw size={12} className={busy === "list" ? "animate-spin" : ""} />
          刷新
        </button>
        <button
          className="btn-primary h-8 px-3 text-[12px] shrink-0 flex items-center gap-1.5"
          disabled={!dir || busy === "import"}
          style={{ opacity: dir ? 1 : 0.45 }}
          onClick={importDir}
          title="递归导入该文件夹（含子目录）里的音频"
        >
          {busy === "import" ? (
            <Loader2 size={12} className="animate-spin" />
          ) : (
            <FolderPlus size={12} />
          )}
          导入该文件夹
        </button>
        {status?.user ? (
          <span
            className="h-8 px-2.5 rounded-lg text-[11.5px] text-[var(--ink-2)] flex items-center gap-1.5 shrink-0"
            style={{ background: "var(--shade)", border: "1px solid var(--line)" }}
            title="已连接的百度账号"
          >
            <User size={12} />
            {status.user}
          </span>
        ) : null}
        <button
          className="btn-secondary h-8 px-2.5 text-[12px] shrink-0 flex items-center gap-1.5"
          onClick={logout}
          title="退出百度网盘登录"
        >
          <LogOut size={12} />
          退出
        </button>
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto">
        {openAlbum ? (
          <div className="pb-3">
            <button
              className="flex items-center gap-1 text-[12px] text-[var(--ink-2)] hover:text-[var(--ink)] mb-2"
              onClick={() => setOpenAlbum(null)}
            >
              <ArrowLeft size={12} /> 返回专辑列表
            </button>
            <div className="text-[13px] text-[var(--ink)] font-medium mb-1.5">
              {openAlbum.album}
              <span className="text-[var(--ink-3)] font-normal ml-2">
                {openAlbum.tracks.length} 首
              </span>
            </div>
            {openAlbum.tracks.map((t) => (
              <div
                key={t.fsId}
                className="group flex items-center gap-2.5 h-10 px-2 rounded-lg hover:bg-[var(--shade)] cursor-default"
              >
                <button
                  className="w-6 h-6 rounded-full flex items-center justify-center text-[var(--ink-2)] hover:text-[var(--ink)]"
                  onClick={() => play(t)}
                  title="播放"
                >
                  {playingId === t.fsId ? (
                    <Loader2 size={13} className="animate-spin" />
                  ) : (
                    <Play size={13} />
                  )}
                </button>
                <span className="text-[12.5px] text-[var(--ink)] truncate flex-1">{t.title}</span>
                <span className="text-[11.5px] text-[var(--ink-3)] truncate max-w-[160px]">
                  {t.artist}
                </span>
                <span className="text-[10.5px] text-[var(--ink-3)] uppercase">{t.ext}</span>
                <span className="text-[11px] text-[var(--ink-3)] tabular-nums w-16 text-right">
                  {fmtSize(t.size)}
                </span>
              </div>
            ))}
          </div>
        ) : (
          <>
            {dirs.map((d) => (
              <div
                key={d.fsId}
                className="group flex items-center gap-2.5 h-10 px-2 rounded-lg hover:bg-[var(--shade)] cursor-pointer"
                onClick={() => loadDir(d.path)}
              >
                <FolderClosed size={15} className="text-[var(--accent-strong)] shrink-0" />
                <span className="text-[12.5px] text-[var(--ink)] truncate flex-1">{d.name}</span>
                <ChevronRight
                  size={13}
                  className="text-[var(--ink-3)] opacity-0 group-hover:opacity-100"
                />
              </div>
            ))}

            {files.map((f) => (
              <div
                key={f.fsId}
                className="group flex items-center gap-2.5 h-10 px-2 rounded-lg hover:bg-[var(--shade)]"
              >
                <button
                  className="w-6 h-6 rounded-full flex items-center justify-center text-[var(--ink-2)] hover:text-[var(--ink)]"
                  onClick={() => preview(f)}
                  title="试听"
                >
                  {playingId === f.fsId ? (
                    <Loader2 size={13} className="animate-spin" />
                  ) : (
                    <Play size={13} />
                  )}
                </button>
                <Music2 size={13} className="text-[var(--ink-3)] shrink-0" />
                <span className="text-[12.5px] text-[var(--ink)] truncate flex-1">{f.name}</span>
                <span className="text-[10.5px] text-[var(--ink-3)] uppercase">{f.ext}</span>
                <span className="text-[11px] text-[var(--ink-3)] tabular-nums w-16 text-right">
                  {fmtSize(f.size)}
                </span>
              </div>
            ))}

            {!dirs.length && !files.length && (
              <div className="h-24 flex items-center justify-center text-[12.5px] text-[var(--ink-3)]">
                {busy === "list" ? "读取目录…" : "此目录下没有音频，进入子文件夹看看吧"}
              </div>
            )}

            {/* 已导入 */}
            <div className="mt-4 pt-3 border-t border-[var(--line)]">
              <div className="flex items-center gap-2 mb-2">
                <span className="text-[12px] text-[var(--ink-2)] font-medium">已导入音乐</span>
                <span className="text-[11px] text-[var(--ink-3)]">
                  {albums.reduce((n, a) => n + a.count, 0)} 首 / {albums.length} 张专辑
                </span>
              </div>

              {roots.length > 0 && (
                <div className="flex flex-wrap gap-1.5 mb-2.5">
                  {roots.map((r) => (
                    <span
                      key={r.root}
                      className="flex items-center gap-1.5 h-6 pl-2 pr-1 rounded-full bg-[var(--shade)] border border-[var(--line)] text-[11px] text-[var(--ink-2)]"
                      title={r.root}
                    >
                      <span className="truncate max-w-[220px]">{r.root}</span>
                      <span className="text-[var(--ink-3)]">{r.count} 首</span>
                      <button
                        className="w-4 h-4 rounded-full flex items-center justify-center hover:bg-[var(--line)] text-[var(--ink-3)]"
                        onClick={() => removeRoot(r.root)}
                        title="移除该目录导入的曲目"
                      >
                        <Trash2 size={10} />
                      </button>
                    </span>
                  ))}
                </div>
              )}

              {albums.length === 0 ? (
                <div className="text-[12px] text-[var(--ink-3)] py-3">
                  还没有导入。进入「我的资源/音乐/V家专辑/专辑名」这类目录后点「导入该文件夹」。
                </div>
              ) : (
                <div className="grid grid-cols-2 gap-1">
                  {albums.map((a) => (
                    <div
                      key={`${a.album}-${a.artist}`}
                      className="flex items-center gap-2.5 h-11 px-2 rounded-lg hover:bg-[var(--shade)] cursor-pointer"
                      onClick={() => openOne(a)}
                    >
                      <div
                        className="w-8 h-8 rounded-lg flex items-center justify-center shrink-0"
                        style={{
                          background: "rgba(255,255,255,0.04)",
                          border: "1px solid rgba(255,255,255,0.06)",
                        }}
                      >
                        <AlbumIcon size={14} className="text-[var(--ink-3)]" />
                      </div>
                      <div className="min-w-0 flex-1">
                        <div className="text-[12.5px] text-[var(--ink)] truncate">{a.album}</div>
                        <div className="text-[11px] text-[var(--ink-3)] truncate">
                          {a.artist || "未知歌手"} · {a.count} 首
                        </div>
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
