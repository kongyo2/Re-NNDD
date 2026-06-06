import { test, expect } from '@playwright/test';
import { installTauriMock, type DownloadItem } from './helpers/tauri-mock';

// The download queue (src/routes/downloads/+page.svelte) is a good stateful
// target: enqueue/start/cancel mutate a queue that list_downloads re-reads. The
// mock's default handlers implement that mutation, so the UI behaves end-to-end.

function pending(id: number, videoId: string): DownloadItem {
  return {
    id,
    videoId,
    status: 'pending',
    progress: 0,
    errorMessage: null,
    scheduledAt: null,
    startedAt: null,
    finishedAt: null,
    retryCount: 0,
  };
}

test.describe('Download queue', () => {
  test('starts empty, then enqueues a video id', async ({ page }) => {
    const mock = await installTauriMock(page);
    await page.goto('/downloads');

    await expect(
      page.getByText('キューは空です。動画 ID を入れて追加してください。'),
    ).toBeVisible();

    await page.getByPlaceholder('動画 ID (例: sm9)').fill('sm9');
    await page.getByRole('button', { name: 'キューに追加' }).click();

    // A queue row for sm9 appears, in the "待機中" (pending) state.
    const queue = page.locator('table.queue');
    await expect(queue.getByText('sm9')).toBeVisible();
    await expect(queue.getByText('待機中')).toBeVisible();
    expect(mock.state.downloads.map((d) => d.videoId)).toContain('sm9');
  });

  test('rejects a non-alphanumeric video id without enqueuing', async ({ page }) => {
    const mock = await installTauriMock(page);
    await page.goto('/downloads');

    await page.getByPlaceholder('動画 ID (例: sm9)').fill('bad id!');
    await page.getByRole('button', { name: 'キューに追加' }).click();

    await expect(page.getByText('動画 ID は英数字のみ（例: sm9, so12345）')).toBeVisible();
    expect(mock.state.calls.some((c) => c.cmd === 'enqueue_download')).toBe(false);
  });

  test('starts a pending download (pending → DL 中)', async ({ page }) => {
    const mock = await installTauriMock(page, {
      state: { downloads: [pending(1, 'so12345')], nextDownloadId: 2 },
    });
    await page.goto('/downloads');

    await expect(page.locator('table.queue').getByText('待機中')).toBeVisible();
    await page.getByRole('button', { name: 'DL 開始' }).click();

    await expect(page.locator('table.queue').getByText('DL 中')).toBeVisible();
    expect(mock.state.downloads[0].status).toBe('downloading');
  });

  test('cancels a queued download after confirming', async ({ page }) => {
    const mock = await installTauriMock(page, {
      state: { downloads: [pending(1, 'sm9')], nextDownloadId: 2 },
    });
    await page.goto('/downloads');

    await expect(page.locator('table.queue').getByText('sm9')).toBeVisible();

    // onCancel() calls confirm(); accept it.
    page.once('dialog', (dialog) => void dialog.accept());
    await page.getByRole('button', { name: '削除' }).click();

    await expect(
      page.getByText('キューは空です。動画 ID を入れて追加してください。'),
    ).toBeVisible();
    expect(mock.state.downloads).toHaveLength(0);
  });
});
