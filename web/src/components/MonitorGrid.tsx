import { createSignal, onCleanup, For, Show, type Component } from "solid-js";
import MonitorCard from "./MonitorCard";
import { computeReorder, moveByOffset } from "../store";

export interface MonitorGridProps {
  monitors: any[];
  onOpen: (id: number) => void;
  onChanged?: () => void;
  reorderEnabled?: boolean;
  onReorder?: (ids: number[]) => void;
}

const MonitorGrid: Component<MonitorGridProps> = (props) => {
  const [draggingId, setDraggingId] = createSignal<number | null>(null);
  const [dragOrder, setDragOrder] = createSignal<number[] | null>(null);
  const [announce, setAnnounce] = createSignal("");
  let cleanup: (() => void) | null = null;

  const currentIds = () => props.monitors.map((m) => m.id);

  // During a drag, render by the live drag order; otherwise trust the store.
  const displayed = () => {
    const order = dragOrder();
    if (!order) return props.monitors;
    const byId = new Map(props.monitors.map((m) => [m.id, m]));
    return order.map((id) => byId.get(id)).filter(Boolean);
  };

  function onGripDown(id: number, _e: PointerEvent) {
    if (!props.reorderEnabled) return;
    setDraggingId(id);
    setDragOrder(currentIds());
    const move = (ev: PointerEvent) => onMove(ev);
    const up = () => finishDrag(true);
    const cancel = () => finishDrag(false);
    const key = (ev: KeyboardEvent) => { if (ev.key === "Escape") finishDrag(false); };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    window.addEventListener("keydown", key);
    cleanup = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
      window.removeEventListener("keydown", key);
    };
  }

  function onMove(e: PointerEvent) {
    const dragging = draggingId();
    if (dragging == null) return;
    // .dragging has pointer-events:none, so elementsFromPoint sees the card beneath.
    const card = document.elementsFromPoint(e.clientX, e.clientY)
      .find((el) => (el as HTMLElement).dataset?.monitorId) as HTMLElement | undefined;
    if (!card) return;
    const targetId = Number(card.dataset.monitorId);
    if (targetId === dragging) return;
    setDragOrder((ord) => computeReorder(ord ?? currentIds(), dragging, targetId));
  }

  function finishDrag(commit: boolean) {
    cleanup?.(); cleanup = null;
    const order = dragOrder();
    const dragging = draggingId();
    setDraggingId(null);
    setDragOrder(null);
    if (commit && order && dragging != null && order.join(",") !== currentIds().join(",")) {
      props.onReorder?.(order);
    }
  }

  function onGripKey(id: number, e: KeyboardEvent) {
    if (!props.reorderEnabled) return;
    const delta = e.key === "ArrowUp" ? -1 : e.key === "ArrowDown" ? 1 : 0;
    if (delta === 0) return;
    e.preventDefault();
    const ids = currentIds();
    const next = moveByOffset(ids, id, delta);
    if (next.join(",") === ids.join(",")) return;
    props.onReorder?.(next);
    const name = props.monitors.find((m) => m.id === id)?.name ?? "monitor";
    setAnnounce(`Moved ${name} to position ${next.indexOf(id) + 1} of ${next.length}`);
  }

  // A grid disposed mid-drag (rare) must detach its window listeners.
  onCleanup(() => cleanup?.());

  return (
    <Show
      when={props.monitors.length > 0}
      fallback={<div class="empty-state">No monitors match. Add your first monitor to get started.</div>}
    >
      <div class="monitor-grid">
        <For each={displayed()}>
          {(m) => (
            <MonitorCard
              monitor={m}
              onOpen={props.onOpen}
              onChanged={props.onChanged}
              reorderEnabled={props.reorderEnabled}
              dragging={draggingId() === m.id}
              onGripDown={onGripDown}
              onGripKey={onGripKey}
            />
          )}
        </For>
      </div>
      <div class="sr-only" aria-live="polite">{announce()}</div>
    </Show>
  );
};

export default MonitorGrid;
