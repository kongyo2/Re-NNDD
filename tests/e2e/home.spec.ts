import { test, expect } from '@playwright/test';
import { installTauriMock } from './helpers/tauri-mock';

// The home route ("/") is also the app shell: a SvelteKit layout with the dark
// sidebar + nine nav links, plus the landing cards and the live app version
// (which proves the Tauri IPC round-trips). See src/routes/+layout.svelte and
// src/routes/+page.svelte.
test.describe('Home / app shell', () => {
  test('boots the shell with the sidebar brand and all nav links', async ({ page }) => {
    await installTauriMock(page);
    await page.goto('/');

    // The <h1 class="brand"> is the layout's "app is alive" signal.
    await expect(page.getByRole('heading', { name: 'Re:NNDD', level: 1 })).toBeVisible();

    const sidebar = page.locator('.sidebar');
    const navLabels = [
      'ホーム',
      'ローカル',
      'ランキング',
      '検索',
      'プレイリスト',
      'ダウンロード',
      '履歴',
      'NG',
      '設定',
    ];
    for (const label of navLabels) {
      await expect(sidebar.getByRole('link', { name: label, exact: true })).toBeVisible();
    }
  });

  test('renders the app version returned over the (mocked) Tauri IPC', async ({ page }) => {
    const mock = await installTauriMock(page, {
      handlers: { get_app_version: () => '9.9.9-e2e' },
    });
    await page.goto('/');

    // The <dd> under "アプリバージョン" is filled by invoke('get_app_version').
    await expect(page.getByText('9.9.9-e2e')).toBeVisible();
    expect(mock.state.calls.some((c) => c.cmd === 'get_app_version')).toBe(true);
  });

  test('landing cards render and link into their routes', async ({ page }) => {
    await installTauriMock(page);
    await page.goto('/');

    for (const card of ['ローカル', 'ランキング', '検索', 'ダウンロード', '履歴']) {
      await expect(page.getByRole('heading', { name: card, level: 3 })).toBeVisible();
    }

    // Clicking the "検索" card navigates via SvelteKit client routing.
    await page.locator('main').getByRole('link', { name: /検索/ }).first().click();
    await expect(page).toHaveURL(/\/search$/);
    await expect(page.getByRole('heading', { name: 'オンライン検索' })).toBeVisible();
  });
});
