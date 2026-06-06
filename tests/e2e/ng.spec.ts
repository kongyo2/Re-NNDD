import { test, expect } from '@playwright/test';
import { installTauriMock } from './helpers/tauri-mock';

// NG rules (src/routes/ng/+page.svelte) are persisted purely in localStorage
// (key "nndd:ngRules") — no IPC. Each Playwright test gets a fresh context, so
// storage starts empty. The mock is still installed so the layout boots cleanly.

test.describe('NG rules', () => {
  test('adds a rule and lists it', async ({ page }) => {
    await installTauriMock(page);
    await page.goto('/ng');

    await expect(page.getByRole('heading', { name: 'NG ルール' })).toBeVisible();
    await expect(page.getByText('該当するルールはありません。')).toBeVisible();

    // Default target is "コメ本文" with placeholder "NG にする文字列".
    await page.getByPlaceholder('NG にする文字列').fill('スパムワード');
    await page.getByRole('button', { name: '追加', exact: true }).click();

    // The new rule shows up in the table and the counter ticks to 1 / 1.
    await expect(page.getByRole('cell', { name: 'スパムワード' })).toBeVisible();
    await expect(page.getByText('1 / 1 件')).toBeVisible();
  });

  test('validates that the pattern is required', async ({ page }) => {
    await installTauriMock(page);
    await page.goto('/ng');

    await page.getByRole('button', { name: '追加', exact: true }).click();

    await expect(page.getByText('パターンは必須です')).toBeVisible();
    await expect(page.getByText('該当するルールはありません。')).toBeVisible();
  });
});
