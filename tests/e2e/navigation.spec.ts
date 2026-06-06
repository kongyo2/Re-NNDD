import { test, expect } from '@playwright/test';
import { installTauriMock } from './helpers/tauri-mock';

// Exercises the persistent sidebar (src/routes/+layout.svelte): each link uses
// SvelteKit client-side routing, gets the `active` class for the current path,
// and a "← 戻る" button appears on non-root, non-detail routes.
//
// The sidebar label and the page <h2> intentionally differ for two routes
// (ローカル → "ライブラリ", 履歴 → "再生履歴"), which this also pins down.
const ROUTES = [
  { link: 'ローカル', url: /\/library$/, heading: 'ライブラリ' },
  { link: 'ランキング', url: /\/ranking$/, heading: 'ランキング' },
  { link: '検索', url: /\/search$/, heading: 'オンライン検索' },
  { link: 'プレイリスト', url: /\/playlists$/, heading: 'プレイリスト' },
  { link: 'ダウンロード', url: /\/downloads$/, heading: 'ダウンロード' },
  { link: '履歴', url: /\/history$/, heading: '再生履歴' },
  { link: 'NG', url: /\/ng$/, heading: 'NG ルール' },
  { link: '設定', url: /\/settings$/, heading: '設定' },
];

test.describe('Sidebar navigation', () => {
  test('navigates to every route and marks the active link', async ({ page }) => {
    await installTauriMock(page);
    await page.goto('/');

    const sidebar = page.locator('.sidebar');
    for (const route of ROUTES) {
      const link = sidebar.getByRole('link', { name: route.link, exact: true });
      await link.click();
      await expect(page).toHaveURL(route.url);
      await expect(page.getByRole('heading', { name: route.heading, level: 2 })).toBeVisible();
      await expect(link).toHaveClass(/active/);
    }
  });

  test('shows the back button off-root and hides it on home', async ({ page }) => {
    await installTauriMock(page);
    await page.goto('/');

    const backButton = page.getByRole('button', { name: '← 戻る' });
    await expect(backButton).toBeHidden(); // canGoBack === false on "/"

    await page.locator('.sidebar').getByRole('link', { name: '設定', exact: true }).click();
    await expect(page).toHaveURL(/\/settings$/);
    await expect(backButton).toBeVisible(); // canGoBack === true on "/settings"

    // …and it actually goes back to home.
    await backButton.click();
    await expect(page).toHaveURL(/\/$/);
    await expect(page.getByRole('heading', { name: 'Re:NNDD', level: 1 })).toBeVisible();
  });
});
