import { test, expect, type Page } from '@playwright/test';
import { installTauriMock, type MockSetup, type TauriMock } from './helpers/tauri-mock';

// Settings (src/routes/settings/+page.svelte) reads the settings KV
// (get_settings), the env report (get_app_info) and login state
// (session_cookie_status), and writes via set_setting. All faked here.
//
// The settings *sections* live behind `{#if !isLoaded()}` and isLoaded() reads a
// non-reactive flag, so they only render once the layout's loadSettings() has
// resolved — i.e. when you reach /settings via in-app navigation, the way the
// real app does (the window always opens at "/"). openSettings() reproduces that
// path and uses the theme flip as a deterministic "settings applied" signal.
async function openSettings(page: Page, setup: MockSetup = {}): Promise<TauriMock> {
  const mock = await installTauriMock(page, {
    handlers: setup.handlers,
    state: {
      ...setup.state,
      // A non-default theme: once loadSettings() applies it, <html data-theme>
      // flips — a reliable post-load signal (and no fixed wait).
      settings: { 'appearance.theme': 'niconico-classic', ...(setup.state?.settings ?? {}) },
    },
  });
  await page.goto('/');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'niconico-classic');
  await page.locator('.sidebar').getByRole('link', { name: '設定', exact: true }).click();
  await expect(page).toHaveURL(/\/settings$/);
  return mock;
}

test.describe('Settings', () => {
  test('renders sections and the app-info report from the IPC', async ({ page }) => {
    await openSettings(page);

    await expect(page.getByRole('heading', { name: '設定', level: 2 })).toBeVisible();
    for (const section of ['再生', '外観', 'アカウント', 'アプリ情報']) {
      await expect(page.getByRole('heading', { name: section, level: 3 })).toBeVisible();
    }

    // App-info values come straight from the mocked get_app_info(). The report
    // is a <dl>, so pin each value to its <dt> (the identifier string is also a
    // substring of the data/db paths, which would otherwise match 5 elements).
    await expect(page.locator('dt:text-is("バージョン") + dd')).toHaveText('0.1.0');
    await expect(page.locator('dt:text-is("識別子") + dd')).toHaveText('jp.renndd.app');
    await expect(page.locator('dt:text-is("yt-dlp") + dd')).toContainText('2025.01.01');
    // Default fixture is logged out.
    await expect(page.getByText('未ログイン')).toBeVisible();
  });

  test('toggling a boolean setting persists it and shows the override affordance', async ({
    page,
  }) => {
    const mock = await openSettings(page);

    // "続きから再生" (playback.resume_enabled) defaults to true and starts
    // un-overridden (no ↺ in its row).
    const row = page.locator('.setting-row', { hasText: '続きから再生' });
    await expect(row.getByRole('button', { name: '↺' })).toBeHidden();

    await page.getByText('続きから再生', { exact: true }).click();

    await expect.poll(() => mock.state.settings['playback.resume_enabled']).toBe('false');
    // The now-overridden row exposes a "↺ reset to default" button.
    await expect(row.getByRole('button', { name: '↺' })).toBeVisible();
  });

  test('logs in via email + password', async ({ page }) => {
    await openSettings(page, {
      handlers: {
        login_password: (_args, state) => {
          state.loggedIn = true;
          return { kind: 'success' };
        },
      },
    });

    await page.getByLabel('メールアドレス / 電話番号').fill('user@test.example');
    await page.getByLabel('パスワード', { exact: true }).fill('hunter2');
    await page.getByRole('button', { name: 'ログイン' }).click();

    await expect(page.getByText('ログインしました。')).toBeVisible();
    await expect(page.getByText('ログイン済み（メモリ内）')).toBeVisible();
  });
});
