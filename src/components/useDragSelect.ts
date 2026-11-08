import { useCallback, useEffect, useRef, useState } from "react";

interface Options {
  /** 仅在批量管理模式下启用 */
  enabled: boolean;
  /** 行在视图中的顺序（rowid 列表），用于计算框选区间 */
  rowids: number[];
  /** 当前已选中的 rowid */
  sel: Set<number>;
  setSel: (next: Set<number>) => void;
  /** 拖拽开始/结束时回调（用来临时关掉行悬停效果） */
  onDragChange?: (dragging: boolean) => void;
}

/**
 * 批量管理里的「按住拖动多选」。
 *
 * 交互：在某一行按下鼠标 → 该行立即反选，并把它记为锚点；
 * 之后把鼠标拖过其它行 → 锚点到当前行之间的所有行一起选中/取消（
 * 方向跟随第一行的动作，和访达/资源管理器的框选一致）；
 * 松开鼠标结束。
 */
export function useDragSelect({ enabled, rowids, sel, setSel, onDragChange }: Options) {
  const dragging = useRef(false);
  const anchor = useRef<number | null>(null);
  const base = useRef<Set<number>>(new Set());
  const selectMode = useRef(true);
  const [, force] = useState(0);

  const end = useCallback(() => {
    if (!dragging.current) return;
    dragging.current = false;
    anchor.current = null;
    document.body.style.removeProperty("user-select");
    onDragChange?.(false);
    force((n) => n + 1);
  }, [onDragChange]);

  useEffect(() => {
    if (!enabled) {
      end();
      return;
    }
    const up = () => end();
    window.addEventListener("mouseup", up);
    window.addEventListener("blur", up);
    return () => {
      window.removeEventListener("mouseup", up);
      window.removeEventListener("blur", up);
    };
  }, [enabled, end]);

  /** 行上的 onMouseDown */
  const onRowMouseDown = useCallback(
    (rowid: number, ev: React.MouseEvent) => {
      if (!enabled || ev.button !== 0) return;
      // 复选框、按钮、输入框等交互元素交给自己处理
      if ((ev.target as HTMLElement).closest("button, input, textarea, a, [data-no-drag-select]")) {
        return;
      }
      ev.preventDefault();
      if (window.getSelection()?.removeAllRanges) window.getSelection()!.removeAllRanges();
      const idx = rowids.indexOf(rowid);
      if (idx < 0) return;
      const next = new Set(sel);
      selectMode.current = !next.has(rowid);
      if (selectMode.current) next.add(rowid);
      else next.delete(rowid);
      base.current = next;
      anchor.current = idx;
      dragging.current = true;
      document.body.style.setProperty("user-select", "none");
      onDragChange?.(true);
      setSel(next);
      force((n) => n + 1);
    },
    [enabled, rowids, sel, setSel, onDragChange],
  );

  /** 行上的 onMouseEnter */
  const onRowEnter = useCallback(
    (rowid: number) => {
      if (!dragging.current || !enabled) return;
      const idx = rowids.indexOf(rowid);
      const from = anchor.current;
      if (idx < 0 || from == null) return;
      const lo = Math.min(from, idx);
      const hi = Math.max(from, idx);
      const next = new Set(base.current);
      for (let i = lo; i <= hi; i++) {
        const id = rowids[i];
        if (id == null) continue;
        if (selectMode.current) next.add(id);
        else next.delete(id);
      }
      setSel(next);
    },
    [enabled, rowids, setSel],
  );

  return { onRowMouseDown, onRowEnter, dragging: dragging.current };
}
