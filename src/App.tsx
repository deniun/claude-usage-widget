import { useEffect, useRef, useState } from "react";
import { cursorPosition, getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import Header from "./components/Header";
import ProviderCard from "./components/ProviderCard";
import SettingsMenu from "./components/SettingsMenu";
import MiniGauge from "./components/MiniGauge";
import { ipc, type ProviderSnapshot } from "./lib/ipc";
import type { Provider, Settings, ViewMode } from "./lib/types";

const PROVIDER_ORDER: Provider[] = ["claude", "codex", "gemini"];
const PROVIDER_LABELS: Record<Provider, string> = {
  claude: "Claude",
  codex: "Codex",
  gemini: "Gemini",
};
const PROVIDER_CODE: Record<Provider, string> = {
  claude: "C",
  codex: "X",
  gemini: "G",
};

const WIDTH_BY_MODE: Record<ViewMode, number> = {
  normal: 320,
  mini: 160,
  super: 210,
};

type SnapshotMap = Partial<Record<Provider, ProviderSnapshot>>;

// 가로 스트립(super)용 축약 라벨: "HH:MM·N분".
function shortRetry(retryAt?: string): string | null {
  if (!retryAt) return null;
  const t = new Date(retryAt).getTime();
  if (Number.isNaN(t)) return null;
  const hh = new Date(t).toLocaleTimeString("ko-KR", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
  const diffMin = Math.ceil((t - Date.now()) / 60000);
  return diffMin > 0 ? `${hh}·${diffMin}분` : hh;
}

export default function App() {
  const [snapshots, setSnapshots] = useState<SnapshotMap>({});
  const [settings, setSettings] = useState<Settings | null>(null);
  const [refreshing, setRefreshing] = useState<Record<Provider, boolean>>({
    claude: false, codex: false, gemini: false,
  });
  const [menuOpen, setMenuOpen] = useState(false);
  const [activeTab, setActiveTab] = useState<Provider>("claude");
  const [, setTick] = useState(0);
  const [titleBarVisible, setTitleBarVisible] = useState(false);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const hideGuardUntilRef = useRef(0);
  const titleBarVisibleRef = useRef(false);
  const lastAppliedSizeRef = useRef<{ w: number; h: number } | null>(null);

  useEffect(() => {
    const armHideGuard = (event: MouseEvent) => {
      if (event.button !== 0) return;
      const target = event.target;
      if (!(target instanceof HTMLElement)) return;
      const dragRegion = target.closest("[data-window-drag-region='true']");
      if (!dragRegion || target.closest("button")) return;
      hideGuardUntilRef.current = Date.now() + 400;
      titleBarVisibleRef.current = true;
      setTitleBarVisible(true);
    };

    document.addEventListener("mousedown", armHideGuard, true);
    return () => {
      document.removeEventListener("mousedown", armHideGuard, true);
    };
  }, []);

  useEffect(() => {
    const currentWindow = getCurrentWindow();
    let cancelled = false;
    let pollInFlight = false;
    // 창 사각형은 Moved/Resized 때만 바뀌므로 캐시해 두고, 75ms 폴링은 커서
    // 좌표 하나만 물어본다. 예전엔 매 틱마다 cursorPosition/outerPosition/
    // outerSize 세 번을 IPC로 왕복해서(초당 40회) 유휴 상태에서도 WebView2
    // 파이프 트래픽이 계속 발생했다.
    let rect: { x: number; y: number; width: number; height: number } | null = null;

    const refreshRect = async () => {
      try {
        const [position, size] = await Promise.all([
          currentWindow.outerPosition(),
          currentWindow.outerSize(),
        ]);
        if (cancelled) return;
        rect = { x: position.x, y: position.y, width: size.width, height: size.height };
      } catch {
        rect = null;
      }
    };

    const show = (v: boolean) => {
      titleBarVisibleRef.current = v;
      setTitleBarVisible((prev) => (prev === v ? prev : v));
    };

    const syncTitleBarVisibility = async () => {
      if (pollInFlight) return;
      pollInFlight = true;
      try {
        if (!rect) await refreshRect();
        const cursor = await cursorPosition();
        if (cancelled || !rect) return;
        const position = rect;
        const size = rect;
        const hovered =
          cursor.x >= position.x &&
          cursor.x < position.x + size.width &&
          cursor.y >= position.y &&
          cursor.y < position.y + size.height;
        const shouldShow =
          menuOpen ||
          hovered ||
          Date.now() < hideGuardUntilRef.current;
        show(shouldShow);
      } catch {
        if (!cancelled && Date.now() >= hideGuardUntilRef.current && !menuOpen) {
          show(false);
        }
      } finally {
        pollInFlight = false;
      }
    };

    // 폴링 주기를 상황에 맞춘다. 타이틀바가 떠 있거나 드래그 보호 구간이면
    // "언제 나가는지"를 봐야 하므로 75ms, 커서가 밖에 있으면 아무것도 바뀔 수
    // 없으므로 1초로 늦춘다. 고정 75ms였을 때는 유휴 상태에서도 초당 13회
    // IPC가 돌아 WebView2 파이프 트래픽의 97%를 혼자 만들어냈다.
    const IDLE_MS = 1000;
    const ACTIVE_MS = 75;
    let timer: number | undefined;
    const nextDelay = () =>
      titleBarVisibleRef.current || Date.now() < hideGuardUntilRef.current
        ? ACTIVE_MS
        : IDLE_MS;
    const schedule = () => {
      if (cancelled) return;
      timer = window.setTimeout(tick, nextDelay());
    };
    const tick = async () => {
      await syncTitleBarVisibility();
      schedule();
    };

    // 커서가 들어오는 순간은 DOM 이벤트로 즉시 잡는다 — 유휴 폴링이 1초라도
    // 타이틀바가 늦게 뜨지 않도록. 이벤트는 웹뷰 안에서 처리되므로 IPC가 없다.
    const wake = () => {
      if (cancelled || titleBarVisibleRef.current) return;
      show(true);
      // 다음 폴링이 실제 커서 위치로 판정을 정정한다(창 밖으로 나갔으면 곧 숨김).
      if (timer !== undefined) window.clearTimeout(timer);
      schedule();
    };
    document.addEventListener("mouseenter", wake, true);
    document.addEventListener("mousemove", wake, true);

    // 이벤트 payload를 그대로 쓰지 않고 다시 질의한다 — onResized는 inner size를
    // 주는데 여기 비교는 outer size 기준이라 값이 어긋난다.
    const unlistenMoved = currentWindow.onMoved(() => { void refreshRect(); });
    const unlistenResized = currentWindow.onResized(() => { void refreshRect(); });

    void refreshRect().then(tick);
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
      document.removeEventListener("mouseenter", wake, true);
      document.removeEventListener("mousemove", wake, true);
      unlistenMoved.then((u) => u());
      unlistenResized.then((u) => u());
    };
  }, [menuOpen]);

  useEffect(() => {
    const id = setInterval(() => setTick((t) => t + 1), 1000);
    return () => clearInterval(id);
  }, []);

  const fetchActive = async (force = false) => {
    setRefreshing((r) => ({ ...r, [activeTab]: true }));
    try {
      const snap = await ipc.getProviderUsage(activeTab, force);
      setSnapshots((s) => ({ ...s, [activeTab]: snap }));
    } finally {
      setRefreshing((r) => ({ ...r, [activeTab]: false }));
    }
  };

  useEffect(() => {
    ipc.getSettings().then((s) => {
      setSettings(s);
      document.documentElement.style.setProperty("--widget-opacity", String(s.opacity));
    });
    ipc.getAllSnapshots().then((all) => {
      const next: SnapshotMap = {};
      for (const key of Object.keys(all) as Provider[]) {
        next[key] = all[key];
      }
      setSnapshots(next);
    });

    const unsubUpdated = ipc.onProviderUpdated((p) => {
      setSnapshots((s) => ({ ...s, [p.provider]: p.snapshot }));
      setRefreshing((r) => ({ ...r, [p.provider]: false }));
    });
    const unsubRefreshing = ipc.onUsageRefreshing((p) => {
      setRefreshing((r) => ({ ...r, [p.provider]: true }));
    });

    const onKey = (e: KeyboardEvent) => {
      if (e.key === "F5") { e.preventDefault(); fetchActive(true); }
      if (e.key === "Escape") { getCurrentWindow().minimize(); }
      if (e.ctrlKey && e.key === "q") { getCurrentWindow().close(); }
    };
    window.addEventListener("keydown", onKey);

    return () => {
      unsubUpdated.then((u) => u());
      unsubRefreshing.then((u) => u());
      window.removeEventListener("keydown", onKey);
    };
  }, []);

  // 주기 폴링은 백엔드 tokio 타이머가 담당하고 usage:provider_updated 이벤트로
  // push한다(위에서 onProviderUpdated로 구독). 여기서는 탭 전환 시 해당 탭을
  // 한 번만 당겨온다(force=false — 신선하면 캐시 즉시 반환, 아니면 갱신).
  useEffect(() => {
    let cancelled = false;
    // 백엔드 폴러가 활성 provider만 갱신하도록 현재 탭을 알린다.
    ipc.setActiveProvider(activeTab).catch(() => {});
    ipc
      .getProviderUsage(activeTab, false)
      .then((snap) => {
        if (!cancelled) setSnapshots((s) => ({ ...s, [activeTab]: snap }));
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [activeTab]);

  useEffect(() => {
    if (settings) {
      document.documentElement.style.setProperty("--widget-opacity", String(settings.opacity));
    }
  }, [settings?.opacity]);

  const viewMode: ViewMode = settings?.viewMode ?? "normal";
  const baseWidth = WIDTH_BY_MODE[viewMode];
  const effectiveWidth = menuOpen ? Math.max(baseWidth, 260) : baseWidth;

  useEffect(() => {
    if (!rootRef.current) return;
    const apply = async (el: HTMLElement) => {
      const h = el.scrollHeight;
      const withMenuH = menuOpen ? Math.max(h, 360) : h;
      const clampedH = Math.max(40, Math.min(900, Math.ceil(withMenuH)));
      const last = lastAppliedSizeRef.current;
      // Skip redundant resizes: on a transparent + decorations:false window,
      // every setSize can briefly flash the native Windows title bar until the
      // next repaint. Only resize when the dimensions actually changed.
      if (last && last.w === effectiveWidth && last.h === clampedH) return;
      lastAppliedSizeRef.current = { w: effectiveWidth, h: clampedH };
      try {
        await getCurrentWindow().setSize(new LogicalSize(effectiveWidth, clampedH));
      } catch {
        lastAppliedSizeRef.current = last;
      }
    };
    apply(rootRef.current);
    const ro = new ResizeObserver((entries) => {
      for (const e of entries) apply(e.target as HTMLElement);
    });
    ro.observe(rootRef.current);
    return () => ro.disconnect();
  }, [effectiveWidth, baseWidth, activeTab, snapshots, viewMode, menuOpen]);

  const activeSnap = snapshots[activeTab];
  const activeFetchedAt = activeSnap ? new Date(activeSnap.fetchedAt) : null;
  const activeLabel = (() => {
    if (!activeFetchedAt) return "—";
    const diffSec = Math.floor((Date.now() - activeFetchedAt.getTime()) / 1000);
    if (diffSec < 60) return `${diffSec}초 전`;
    if (diffSec < 3600) return `${Math.floor(diffSec / 60)}분 전`;
    return `${Math.floor(diffSec / 3600)}시간 전`;
  })();

  const cycleProvider = () => {
    const idx = PROVIDER_ORDER.indexOf(activeTab);
    setActiveTab(PROVIDER_ORDER[(idx + 1) % PROVIDER_ORDER.length]);
  };

  const renderTabs = (compact: boolean) => (
    <div className={`flex border-b border-border/40 bg-surface-light/30 ${compact ? "text-[10px]" : ""}`}>
      {PROVIDER_ORDER.map((p) => (
        <button
          key={p}
          onClick={() => setActiveTab(p)}
          className={`flex-1 ${compact ? "px-1.5 py-1" : "px-2 py-1.5"} text-xs transition-colors ${
            activeTab === p
              ? "text-text border-b-2 border-accent -mb-[1px]"
              : "text-text-dim hover:text-text"
          }`}
        >
          {PROVIDER_LABELS[p]}
        </button>
      ))}
    </div>
  );

  const containerBase = "relative w-full flex flex-col";

  const handleBodyDragMouseDown = async (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    const el = e.target as HTMLElement | null;
    if (!el) return;
    if (el.closest("button, a, input, textarea, select, code, [data-no-drag]")) return;
    await getCurrentWindow().startDragging();
  };
  const bgStyle = { backgroundColor: `rgba(26, 26, 26, var(--widget-opacity, 0.92))` };

  const fadeClass = titleBarVisible ? "opacity-100" : "opacity-0 pointer-events-none";
  const header = (
    <div
      className={`transition-opacity duration-150 overflow-hidden rounded-t-xl border-x border-t border-border/60 ${fadeClass}`}
      style={bgStyle}
    >
      <Header
        onRefresh={() => fetchActive(true)}
        refreshing={refreshing[activeTab]}
        onOpenMenu={() => setMenuOpen(true)}
        lastUpdatedAt={activeFetchedAt}
        compact={viewMode !== "normal"}
      />
    </div>
  );
  const bodyOnlyClass = `flex flex-col overflow-hidden border-x border-border/60 ${
    titleBarVisible ? "" : "rounded-xl border-y"
  }`;

  if (viewMode === "super") {
    return (
      <div
        ref={rootRef}
        className={containerBase}
        onMouseDown={handleBodyDragMouseDown}
      >
        {header}
        <div
          data-window-drag-region="true"
          className={`flex flex-row items-center gap-2 px-2 py-1.5 overflow-x-auto border-x border-b border-border/60 ${titleBarVisible ? "rounded-b-xl" : "rounded-xl border-t"}`}
          style={bgStyle}
        >
          <button
            onClick={cycleProvider}
            className="flex-shrink-0 inline-flex items-center justify-center w-5 h-5 rounded bg-surface-light/60 text-[10px] font-semibold hover:bg-surface-light"
            title={`${PROVIDER_LABELS[activeTab]} (클릭해서 전환)`}
          >
            {PROVIDER_CODE[activeTab]}
          </button>
          {activeSnap && activeSnap.response.windows.length > 0 ? (
            activeSnap.response.windows.map((w) => (
              <MiniGauge key={w.key} window={w} provider={activeTab} />
            ))
          ) : activeSnap?.response.status === "expired" ? (
            <button
              onClick={async () => {
                try {
                  await ipc.refreshViaCli(activeTab);
                  const snap = await ipc.getProviderUsage(activeTab, true);
                  setSnapshots((s) => ({ ...s, [activeTab]: snap }));
                } catch (e) {
                  console.error("CLI refresh failed", e);
                }
              }}
              className="px-1.5 py-0.5 text-[10px] rounded bg-accent/20 hover:bg-accent/30 text-text"
              title="CLI로 토큰 갱신"
            >
              만료 ↻
            </button>
          ) : (
            <span className="text-[10px] text-text-dim">
              {activeSnap?.response.status === "not_authenticated" ? "로그인 안됨" : "—"}
            </span>
          )}
          {activeSnap?.response.status === "rate_limited" && activeSnap.retryAt && (
            <span
              className="flex-shrink-0 text-[9px] text-yellow-500/90 whitespace-nowrap"
              title="요청 제한 — 다음 요청 시각"
            >
              ↻ {shortRetry(activeSnap.retryAt)}
            </span>
          )}
        </div>
        {menuOpen && settings && (
          <SettingsMenu settings={settings} onChange={setSettings} onClose={() => setMenuOpen(false)} />
        )}
      </div>
    );
  }

  const compact = viewMode === "mini";

  return (
    <div
      ref={rootRef}
      className={containerBase}
      onMouseDown={handleBodyDragMouseDown}
    >
      {header}
      <div
        className={`transition-opacity duration-150 overflow-hidden border-x border-border/60 ${fadeClass}`}
        style={bgStyle}
      >
        {renderTabs(compact)}
      </div>
      <div className={bodyOnlyClass} style={bgStyle}>
        <div
          className={compact ? "px-1.5 py-1.5" : "px-3 py-3"}
          style={compact ? { zoom: 0.75 } : undefined}
        >
          {!activeSnap && <div className="text-xs text-text-dim text-center py-4">로딩 중...</div>}
          {activeSnap && <ProviderCard data={activeSnap.response} retryAt={activeSnap.retryAt} />}
        </div>
      </div>
      <div
        className={`transition-opacity duration-150 overflow-hidden rounded-b-xl border-x border-b border-border/60 ${fadeClass}`}
        style={bgStyle}
      >
        <div className={`${compact ? "px-2 py-1" : "px-3 py-1.5"} border-t border-border/40 text-[10px] text-text-dim text-right`}>
          마지막 갱신: {activeLabel}
        </div>
      </div>
      {menuOpen && settings && (
        <SettingsMenu settings={settings} onChange={setSettings} onClose={() => setMenuOpen(false)} />
      )}
    </div>
  );
}
