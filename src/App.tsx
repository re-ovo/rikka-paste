import { useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

type Config = {
  deviceId: string;
  deviceName: string;
  syncKey: string;
  enabled: boolean;
  recordHistory: boolean;
};

type Peer = { id: string; name: string; addr: string; matched: boolean };

type LogEntry = {
  ts: number;
  incoming: boolean;
  peer: string;
  preview: string;
  error: string | null;
};

type Snapshot = {
  config: Config;
  fingerprint: string;
  peers: Peer[];
  log: LogEntry[];
};

function App() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [name, setName] = useState("");
  const [key, setKey] = useState("");
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  const [scrolled, setScrolled] = useState(false);

  useEffect(() => {
    invoke<Snapshot>("get_state").then((s) => {
      setSnapshot(s);
      setName(s.config.deviceName);
      setKey(s.config.syncKey);
    });
    const unlisten = listen<Snapshot>("state-changed", (e) => setSnapshot(e.payload));
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  if (!snapshot) return null;
  const { config, peers, log } = snapshot;
  const dirty = name !== config.deviceName || key !== config.syncKey;

  async function apply(patch: Partial<Config>) {
    try {
      const s = await invoke<Snapshot>("update_config", {
        config: { ...snapshot!.config, ...patch },
      });
      setSnapshot(s);
      setName(s.config.deviceName);
      setKey(s.config.syncKey);
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }

  async function copyKey() {
    await invoke("copy_secret", { text: key });
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }

  const matchedCount = peers.filter((p) => p.matched).length;

  return (
    <div className="relative flex h-screen flex-col text-sm">
      <TitleBar scrolled={scrolled} />

      <main className="flex-1 overflow-y-auto" onScroll={(e) => setScrolled(e.currentTarget.scrollTop > 0)}>
        <section className="mx-auto max-w-xl px-5 pt-14 pb-2">
          <div className="flex items-end justify-between gap-4">
            <div className="min-w-0">
              <p className="text-xs font-medium tracking-wide text-zinc-500">Rikka Paste</p>
              <h1 className="mt-1 text-2xl font-semibold">
                {config.enabled ? `正在与 ${matchedCount} 台设备同步` : "同步已暂停"}
              </h1>
            </div>
            <Switch checked={config.enabled} onChange={(enabled) => apply({ enabled })} />
          </div>

          <div className="mt-4 flex flex-col gap-1">
            <HeroRow label="本机">
              <input className={inlineInputClass} value={name} onChange={(e) => setName(e.target.value)} />
            </HeroRow>
            <HeroRow label="配对码">
              <input
                className={`${inlineInputClass} font-mono tracking-wider select-text`}
                value={key}
                spellCheck={false}
                onChange={(e) => setKey(e.target.value)}
              />
              <TextButton onClick={async () => setKey(await invoke<string>("generate_key"))}>生成</TextButton>
              <TextButton onClick={copyKey}>{copied ? "已复制" : "复制"}</TextButton>
            </HeroRow>
          </div>
          <p className="mt-2 text-xs text-zinc-500">在其他设备上填入相同的配对码即可互相同步</p>
          {error && <p className="mt-2 text-xs text-red-500">{error}</p>}
          {dirty && (
            <div className="mt-3 flex justify-end gap-2">
              <Button
                onClick={() => {
                  setName(config.deviceName);
                  setKey(config.syncKey);
                }}
              >
                取消
              </Button>
              <Button primary onClick={() => apply({ deviceName: name, syncKey: key })}>
                保存
              </Button>
            </div>
          )}
        </section>

        <div className="mx-auto flex max-w-xl flex-col gap-4 p-4">
          <Card title={`局域网设备 · ${peers.length}`}>
            {peers.length === 0 ? (
              <p className="py-2 text-zinc-500">正在查找运行 Rikka Paste 的设备…</p>
            ) : (
              <ul className="-my-1 divide-y divide-zinc-100 dark:divide-zinc-800">
                {peers.map((p) => (
                  <li key={p.id} className="flex items-center gap-3 py-2">
                    <span
                      className={`size-2 rounded-full ${p.matched ? "bg-emerald-500" : "bg-amber-500"}`}
                    />
                    <span className="flex-1 truncate">{p.name}</span>
                    {p.matched ? (
                      <span className="font-mono text-xs text-zinc-500">{p.addr}</span>
                    ) : (
                      <span className="text-xs text-amber-600 dark:text-amber-400">配对码不同</span>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </Card>

          <Card title="选项">
            <label className="flex cursor-pointer items-start justify-between gap-4">
              <div>
                <p>记入剪贴板历史</p>
                <p className="text-xs text-zinc-500">
                  关闭后，从其他设备同步来的内容不会出现在 Maccy / Win+V 等剪贴板历史中
                </p>
              </div>
              <Switch checked={config.recordHistory} onChange={(recordHistory) => apply({ recordHistory })} />
            </label>
          </Card>

          <Card title="最近同步">
            {log.length === 0 ? (
              <p className="py-2 text-zinc-500">还没有同步记录</p>
            ) : (
              <ul className="-my-1 divide-y divide-zinc-100 dark:divide-zinc-800">
                {log.map((entry, i) => (
                  <li key={`${entry.ts}-${i}`} className="py-2">
                    <div className="flex items-center gap-2 text-xs text-zinc-500">
                      <span className={entry.incoming ? "text-sky-500" : "text-violet-500"}>
                        {entry.incoming ? "↓ 来自" : "↑ 发往"}
                      </span>
                      <span className="flex-1 truncate">{entry.peer}</span>
                      <span className="tabular-nums">{new Date(entry.ts).toLocaleTimeString()}</span>
                    </div>
                    <p className="truncate">{entry.preview}</p>
                    {entry.error && <p className="text-xs text-red-500">{entry.error}</p>}
                  </li>
                ))}
              </ul>
            )}
          </Card>
        </div>
      </main>
    </div>
  );
}

const isMac = navigator.userAgent.includes("Mac");

/// 沉浸式标题栏：透明的拖动区叠在内容上方，滚动后才出现背景。
/// macOS 由系统绘制红绿灯，Windows 自绘最小化/关闭按钮。
function TitleBar({ scrolled }: { scrolled: boolean }) {
  return (
    <header
      data-tauri-drag-region
      className={`absolute inset-x-0 top-0 z-10 flex h-11 justify-end transition-colors ${
        scrolled ? "bg-zinc-50/80 backdrop-blur dark:bg-zinc-950/80" : ""
      }`}
    >
      {!isMac && <WindowControls />}
    </header>
  );
}

function HeroRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex items-center gap-1">
      <span className="w-12 shrink-0 text-xs text-zinc-500">{label}</span>
      {children}
    </div>
  );
}

function TextButton({ onClick, children }: { onClick: () => void; children: ReactNode }) {
  return (
    <button
      className="shrink-0 rounded-md px-2 py-1 text-xs text-zinc-500 hover:bg-zinc-200/70 hover:text-zinc-900 dark:hover:bg-zinc-800 dark:hover:text-zinc-100"
      onClick={onClick}
    >
      {children}
    </button>
  );
}

function WindowControls() {
  const win = getCurrentWindow();
  const base = "flex h-11 w-12 items-center justify-center text-zinc-600 dark:text-zinc-300";
  return (
    <div className="flex">
      <button className={`${base} hover:bg-zinc-200 dark:hover:bg-zinc-800`} onClick={() => win.minimize()}>
        <svg width="10" height="10" viewBox="0 0 10 10" stroke="currentColor">
          <path d="M0 5h10" />
        </svg>
      </button>
      {/* close 会触发 CloseRequested，后端只隐藏窗口 */}
      <button className={`${base} hover:bg-red-600 hover:text-white`} onClick={() => win.close()}>
        <svg width="10" height="10" viewBox="0 0 10 10" stroke="currentColor">
          <path d="M0 0l10 10M10 0L0 10" />
        </svg>
      </button>
    </div>
  );
}

const inlineInputClass =
  "min-w-0 flex-1 rounded-md border border-transparent bg-transparent px-2 py-1 outline-none hover:border-zinc-200 focus:border-zinc-400 focus:bg-white dark:hover:border-zinc-700 dark:focus:border-zinc-500 dark:focus:bg-zinc-900";

function Card({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="rounded-xl border border-zinc-200 bg-white p-4 dark:border-zinc-800 dark:bg-zinc-900">
      <h2 className="mb-3 text-xs font-medium text-zinc-500">{title}</h2>
      <div className="flex flex-col gap-3">{children}</div>
    </section>
  );
}

function Button({
  primary,
  onClick,
  children,
}: {
  primary?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  const style = primary
    ? "bg-zinc-900 text-white hover:bg-zinc-700 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-300"
    : "border border-zinc-200 hover:bg-zinc-100 dark:border-zinc-700 dark:hover:bg-zinc-800";
  return (
    <button className={`shrink-0 rounded-md px-3 py-1.5 ${style}`} onClick={onClick}>
      {children}
    </button>
  );
}

function Switch({ checked, onChange }: { checked: boolean; onChange: (checked: boolean) => void }) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className={`relative h-6 w-10 shrink-0 rounded-full transition-colors ${
        checked ? "bg-emerald-500" : "bg-zinc-300 dark:bg-zinc-700"
      }`}
    >
      <span
        className={`absolute top-0.5 left-0.5 size-5 rounded-full bg-white shadow transition-transform ${
          checked ? "translate-x-4" : ""
        }`}
      />
    </button>
  );
}

export default App;
