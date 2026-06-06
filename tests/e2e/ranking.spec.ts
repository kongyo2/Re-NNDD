import { test, expect } from '@playwright/test';
import { installTauriMock, buildRankingHtml } from './helpers/tauri-mock';

// Ranking (src/routes/ranking/+page.svelte) invokes 'fetch_ranking_html' and runs
// the response through @kongyo2/nicoran-api's HTML→JSON parser before rendering.
// buildRankingHtml() produces a schema-valid document so the full fetch → parse →
// render path runs for real.

test.describe('Ranking', () => {
  test('renders genre/term controls and parsed ranking items', async ({ page }) => {
    const mock = await installTauriMock(page, {
      handlers: {
        fetch_ranking_html: () =>
          buildRankingHtml(
            [
              {
                id: 'sm40000',
                title: 'ランキング1位の動画',
                duration: 222,
                count: { view: 99999, comment: 120, mylist: 30, like: 45 },
                owner: { ownerType: 'user', id: '7', name: 'テスト投稿者' },
              },
            ],
            { label: '総合 24時間ランキング' },
          ),
      },
    });
    await page.goto('/ranking');

    await expect(page.getByRole('heading', { name: 'ランキング', level: 2 })).toBeVisible();
    // Genre chip + term tab controls render.
    await expect(page.getByRole('button', { name: '総合' })).toBeVisible();
    await expect(page.getByRole('button', { name: '24時間' })).toBeVisible();

    // The parsed item shows up as a player link.
    await expect(page.getByRole('link', { name: 'ランキング1位の動画' })).toBeVisible();

    // …and the UI requested a real niconico ranking URL.
    const call = mock.state.calls.find((c) => c.cmd === 'fetch_ranking_html');
    expect(call).toBeDefined();
    expect(String(call?.args.url)).toContain('nicovideo.jp/ranking');
  });

  test('switching genre triggers a re-fetch', async ({ page }) => {
    const mock = await installTauriMock(page, {
      handlers: { fetch_ranking_html: () => buildRankingHtml([], { label: '総合' }) },
    });
    await page.goto('/ranking');
    await expect(page.getByRole('heading', { name: 'ランキング', level: 2 })).toBeVisible();

    const fetches = () => mock.state.calls.filter((c) => c.cmd === 'fetch_ranking_html').length;
    await expect.poll(fetches).toBeGreaterThanOrEqual(1);

    await page.getByRole('button', { name: 'ゲーム' }).click();
    await expect.poll(fetches).toBeGreaterThanOrEqual(2);
  });
});
