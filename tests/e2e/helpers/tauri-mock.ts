// Tauri IPC mock for Playwright E2E.
//
// Re:NNDD is a SvelteKit SPA that lives inside a Tauri 2 WebView and talks to
// the Rust backend through `invoke()` (`@tauri-apps/api/core`) and `listen()`
// (`@tauri-apps/api/event`). Playwright drives a *real browser* against the Vite
// dev server (`npm run dev`, :1420) — there is no Rust side there, so every
// `invoke` would reject with "window.__TAURI_INTERNALS__ is undefined".
//
// This helper installs a fake `window.__TAURI_INTERNALS__` (the exact shape the
// bundled `@tauri-apps/api` reads — see node_modules/@tauri-apps/api/core.js):
//   - invoke(cmd, args, options)      -> routed to a Node-side dispatcher
//   - transformCallback(cb, once)     -> in-page callback registry (sync)
//   - unregisterCallback(id)          -> in-page (sync)
//   - convertFileSrc(path, protocol)  -> in-page, returns a harmless data URL
//
// `invoke` is the interesting one: rather than baking static fixtures into the
// page, it forwards to a function exposed on the Node side (`exposeFunction`).
// That lets the command handlers be real stateful closures — enqueueing a
// download mutates an array that the next `list_downloads` poll returns, exactly
// like the real backend — and lets a test inspect what was called afterwards.
//
// The pure-JS pieces (`tauri-mock.shim` below) cannot close over Node values, so
// all dynamic data flows through the exposed `__TAURI_MOCK_INVOKE__` binding.

import type { Page } from '@playwright/test';

/** Arguments object Tauri passes alongside a command (already plain JSON). */
export type InvokeArgs = Record<string, unknown>;

/** A command handler: gets the invoke args + shared mutable state, returns the
 *  backend response (sync or async). Throw to simulate a command error — the
 *  rejection propagates to the page exactly like a real `invoke` failure. */
export type InvokeHandler = (args: InvokeArgs, state: TauriMockState) => unknown;

/** Map of Tauri command name -> handler. */
export type InvokeHandlers = Record<string, InvokeHandler>;

/** One recorded `invoke` call, so tests can assert on what the UI requested. */
export type RecordedCall = { cmd: string; args: InvokeArgs };

export type DownloadStatus = 'pending' | 'downloading' | 'done' | 'error' | 'paused';

export type DownloadItem = {
  id: number;
  videoId: string;
  status: DownloadStatus;
  progress: number;
  errorMessage: string | null;
  scheduledAt: number | null;
  startedAt: number | null;
  finishedAt: number | null;
  retryCount: number;
};

export type AppInfo = {
  version: string;
  identifier: string;
  dataDir: string;
  videosDir: string;
  dbPath: string;
  localServerPort: number;
  ytdlpAvailable: boolean;
  ytdlpVersion: string | null;
  ytdlpSource: 'bundled' | 'sidecar' | 'system_path' | 'not_found';
  ytdlpPath: string;
  ffmpegAvailable: boolean;
  ffmpegVersion: string | null;
  ffmpegSource: 'bundled' | 'sidecar' | 'system_path' | 'not_found';
  ffmpegPath: string;
  libraryVideoCount: number;
  libraryVideosSizeBytes: number;
};

/** Mutable fake-backend state shared across every command handler in a test. */
export type TauriMockState = {
  appVersion: string;
  appInfo: AppInfo;
  /** `get_settings` KV store; `set_setting` / `delete_setting` mutate it. */
  settings: Record<string, string>;
  /** Download queue; `enqueue_download` / `cancel_download` / … mutate it. */
  downloads: DownloadItem[];
  /** `list_library_videos` payload. */
  libraryVideos: unknown[];
  /** Drives `session_cookie_status` (login state). */
  loggedIn: boolean;
  /** Auto-increment id for newly enqueued downloads. */
  nextDownloadId: number;
  /** Every command the UI invoked, in order. */
  calls: RecordedCall[];
};

export type MockSetup = {
  /** Per-command handlers, merged over (and overriding) the built-in defaults. */
  handlers?: InvokeHandlers;
  /** Seed values merged over the default {@link TauriMockState}. */
  state?: Partial<TauriMockState>;
};

export type TauriMock = {
  /** Live backend state — assert against it after driving the UI. */
  state: TauriMockState;
};

function defaultAppInfo(version: string): AppInfo {
  return {
    version,
    identifier: 'jp.renndd.app',
    dataDir: '/home/tester/.local/share/jp.renndd.app',
    videosDir: '/home/tester/.local/share/jp.renndd.app/videos',
    dbPath: '/home/tester/.local/share/jp.renndd.app/library.db',
    localServerPort: 49876,
    ytdlpAvailable: true,
    ytdlpVersion: '2025.01.01',
    ytdlpSource: 'bundled',
    ytdlpPath: '/opt/renndd/bin/yt-dlp',
    ffmpegAvailable: true,
    ffmpegVersion: '6.1',
    ffmpegSource: 'system_path',
    ffmpegPath: '/usr/bin/ffmpeg',
    libraryVideoCount: 0,
    libraryVideosSizeBytes: 0,
  };
}

/** A ranking video, as the ranking page consumes it (a loose subset of
 *  @kongyo2/nicoran-api's RankingItem — only what {@link buildRankingHtml} needs). */
export type RankingFixtureItem = {
  id: string;
  title: string;
  registeredAt?: string;
  duration?: number;
  count?: { view?: number; comment?: number; mylist?: number; like?: number };
  thumbnail?: { url?: string };
  owner?: { ownerType?: string; id?: string; name?: string };
};

/**
 * Build the niconico ranking HTML that invoke('fetch_ranking_html') returns.
 *
 * The ranking page feeds the response to `@kongyo2/nicoran-api`'s
 * `extractAndParse`, which pulls a JSON blob out of
 * `<meta name="server-response" content="…">` and validates it against a Zod
 * schema. This produces the minimal schema-valid document wrapping `items`.
 */
export function buildRankingHtml(
  items: RankingFixtureItem[],
  opts: { label?: string; title?: string } = {},
): string {
  const serverResponse = {
    meta: { status: 200 },
    data: {
      metadata: { title: opts.title ?? 'ランキング' },
      response: {
        $getTeibanRanking: {
          data: {
            featuredKey: 'all',
            label: opts.label ?? '総合ランキング',
            maxItemCount: items.length,
            hasNext: false,
            items,
          },
        },
      },
    },
  };
  // Escape for an HTML double-quoted attribute; cheerio decodes it back to JSON.
  const content = JSON.stringify(serverResponse)
    .replace(/&/g, '&amp;')
    .replace(/"/g, '&quot;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
  return `<!doctype html><html><head><meta name="server-response" content="${content}"></head><body></body></html>`;
}

function defaultState(seed: Partial<TauriMockState> = {}): TauriMockState {
  const appVersion = seed.appVersion ?? '0.1.0';
  return {
    appVersion,
    appInfo: seed.appInfo ?? defaultAppInfo(appVersion),
    settings: seed.settings ?? {},
    downloads: seed.downloads ?? [],
    libraryVideos: seed.libraryVideos ?? [],
    loggedIn: seed.loggedIn ?? false,
    nextDownloadId: seed.nextDownloadId ?? 1,
    calls: seed.calls ?? [],
  };
}

/** Built-in handlers covering every command the app fires on its boot path
 *  (layout console bridge + settings load + plugin host) and on each route's
 *  `onMount`, so any page renders without an explicit per-test handler. */
function defaultHandlers(): InvokeHandlers {
  let eventId = 0;
  return {
    // --- Tauri internals / boot path ---------------------------------------
    // consoleBridge forwards every console.* call here; keep it a no-op.
    web_log: () => null,
    // `@tauri-apps/api/event` listen()/unlisten()/emit() funnel through these.
    'plugin:event|listen': () => ++eventId,
    'plugin:event|unlisten': () => null,
    'plugin:event|emit': () => null,
    'plugin:event|emit_to': () => null,
    // Plugin host enumerates installed plugins on bootstrap.
    plugin_list_installed: () => [],

    // --- App info / settings -----------------------------------------------
    get_app_version: (_args, state) => state.appVersion,
    get_app_info: (_args, state) => state.appInfo,
    get_settings: (_args, state) => ({ ...state.settings }),
    set_setting: (args, state) => {
      state.settings[String(args.key)] = String(args.value);
      return null;
    },
    delete_setting: (args, state) => {
      delete state.settings[String(args.key)];
      return null;
    },

    // --- Account -----------------------------------------------------------
    session_cookie_status: (_args, state) => state.loggedIn,
    clear_session_cookie: (_args, state) => {
      state.loggedIn = false;
      return null;
    },

    // --- Library -----------------------------------------------------------
    list_library_videos: (_args, state) => state.libraryVideos,

    // --- Download queue (stateful) -----------------------------------------
    list_downloads: (_args, state) => state.downloads,
    enqueue_download: (args, state) => {
      const item: DownloadItem = {
        id: state.nextDownloadId++,
        videoId: String(args.videoId),
        status: 'pending',
        progress: 0,
        errorMessage: null,
        scheduledAt: (args.scheduledAt as number | null) ?? null,
        startedAt: null,
        finishedAt: null,
        retryCount: 0,
      };
      state.downloads.push(item);
      return item;
    },
    start_download: (args, state) => {
      const item = state.downloads.find((d) => d.id === Number(args.id));
      if (item) {
        item.status = 'downloading';
        item.startedAt = Math.floor(Date.now() / 1000);
      }
      return null;
    },
    cancel_download: (args, state) => {
      const before = state.downloads.length;
      state.downloads = state.downloads.filter((d) => d.id !== Number(args.id));
      return state.downloads.length < before;
    },
    clear_finished_downloads: (_args, state) => {
      const before = state.downloads.length;
      state.downloads = state.downloads.filter((d) => d.status !== 'done' && d.status !== 'error');
      return before - state.downloads.length;
    },

    // --- Search (empty by default; override per test) ----------------------
    search_videos_online: () => ({
      meta: { status: 200, totalCount: 0, id: 'mock' },
      data: [],
    }),

    // --- Ranking (empty but schema-valid by default; override per test) -----
    fetch_ranking_html: () => buildRankingHtml([]),
    search_short_ranking: () => ({ items: [], totalCount: 0, hasNext: false }),
  };
}

/**
 * Install the Tauri IPC mock on `page`. Call once per test, before navigating.
 *
 * @example
 *   const mock = await installTauriMock(page, {
 *     handlers: { get_app_version: () => '9.9.9' },
 *   });
 *   await page.goto('/');
 *   // …drive the UI…
 *   expect(mock.state.calls.some((c) => c.cmd === 'get_app_version')).toBe(true);
 */
export async function installTauriMock(page: Page, setup: MockSetup = {}): Promise<TauriMock> {
  const state = defaultState(setup.state);
  const handlers: InvokeHandlers = { ...defaultHandlers(), ...setup.handlers };

  // Node-side dispatcher. Records the call, runs the matching handler, and
  // returns its (awaited) result. Unknown commands resolve to null so a stray
  // invoke never hard-crashes a page mid-test.
  await page.exposeFunction(
    '__TAURI_MOCK_INVOKE__',
    async (cmd: string, args: InvokeArgs): Promise<unknown> => {
      state.calls.push({ cmd, args });
      const handler = handlers[cmd];
      if (!handler) {
        console.warn(`[tauri-mock] unhandled command: ${cmd}`);
        return null;
      }
      return await handler(args, state);
    },
  );

  // In-page shim. Pure JS: it can only use the exposed binding + browser APIs,
  // never Node closures — that is why all data goes through __TAURI_MOCK_INVOKE__.
  await page.addInitScript(() => {
    type Cb = (payload: unknown) => void;
    interface TauriWindow {
      __TAURI_INTERNALS__?: unknown;
      __TAURI_MOCK_INVOKE__?: (cmd: string, args: InvokeArgs) => Promise<unknown>;
      __TAURI_MOCK_CB__?: Map<number, Cb>;
      isTauri?: boolean;
    }
    const w = window as unknown as TauriWindow;
    const callbacks = new Map<number, Cb>();
    let cbId = 0;
    // 1x1 transparent PNG; convertFileSrc() targets `<img src>`, so resolve to a
    // self-contained data URL instead of a 404-ing fake asset:// URL.
    const BLANK_PNG =
      'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==';

    w.__TAURI_MOCK_CB__ = callbacks;
    w.isTauri = true;
    w.__TAURI_INTERNALS__ = {
      invoke: (cmd: string, args?: InvokeArgs) => w.__TAURI_MOCK_INVOKE__!(cmd, args ?? {}),
      transformCallback: (cb: Cb) => {
        const id = ++cbId;
        callbacks.set(id, cb);
        return id;
      },
      unregisterCallback: (id: number) => {
        callbacks.delete(id);
      },
      convertFileSrc: () => BLANK_PNG,
    };
  });

  return { state };
}
