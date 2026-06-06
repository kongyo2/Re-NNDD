import { test, expect } from '@playwright/test';
import { installTauriMock } from './helpers/tauri-mock';

// Online search (src/routes/search/+page.svelte) calls invoke('search_videos_online')
// on submit and renders a SearchHitCard per hit. We fake that command so the
// real form → request → render → result pipeline runs in a browser.

const HITS = [
  {
    contentId: 'sm1000',
    title: 'テスト動画アルファ',
    viewCounter: 12345,
    commentCounter: 678,
    mylistCounter: 90,
    lengthSeconds: 142,
    thumbnailUrl: 'https://example.test/a.jpg',
    startTime: '2024-01-02T03:04:05+09:00',
    tags: 'ボカロ 音楽',
    userId: 111,
  },
  {
    contentId: 'sm2000',
    title: 'テスト動画ベータ',
    viewCounter: 222,
    commentCounter: 33,
    mylistCounter: 4,
    lengthSeconds: 75,
    thumbnailUrl: 'https://example.test/b.jpg',
    startTime: '2024-02-03T04:05:06+09:00',
    tags: 'ゲーム 実況',
    userId: 222,
  },
];

test.describe('Online search', () => {
  test('submits a query and renders the returned hits', async ({ page }) => {
    const mock = await installTauriMock(page, {
      handlers: {
        search_videos_online: () => ({
          meta: { status: 200, totalCount: HITS.length, id: 'mock-search' },
          data: HITS,
        }),
      },
    });
    await page.goto('/search');

    await page.getByLabel('検索キーワード').fill('ボカロ');
    await page.getByRole('button', { name: '検索' }).click();

    // Both hit titles render as links into the player route.
    await expect(page.getByRole('link', { name: 'テスト動画アルファ' })).toBeVisible();
    await expect(page.getByRole('link', { name: 'テスト動画ベータ' })).toBeVisible();
    // Result meta line reflects the totalCount from the response.
    await expect(page.getByText('合計 2 件')).toBeVisible();

    // The UI actually sent our query through the IPC boundary.
    const call = mock.state.calls.find((c) => c.cmd === 'search_videos_online');
    expect(call).toBeDefined();
    expect((call?.args.query as { q: string }).q).toBe('ボカロ');
  });

  test('shows the empty state when there are no hits', async ({ page }) => {
    await installTauriMock(page, {
      handlers: {
        search_videos_online: () => ({
          meta: { status: 200, totalCount: 0, id: 'mock-empty' },
          data: [],
        }),
      },
    });
    await page.goto('/search');

    await page.getByLabel('検索キーワード').fill('該当しない語');
    await page.getByRole('button', { name: '検索' }).click();

    await expect(page.getByText('該当なし')).toBeVisible();
  });

  test('surfaces a backend error in the alert', async ({ page }) => {
    await installTauriMock(page, {
      handlers: {
        search_videos_online: () => {
          throw new Error('snapshot API exploded');
        },
      },
    });
    await page.goto('/search');

    await page.getByLabel('検索キーワード').fill('エラー');
    await page.getByRole('button', { name: '検索' }).click();

    await expect(page.getByRole('alert')).toContainText('snapshot API exploded');
  });
});
