// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';
import { SETTING_DEFS } from './settings.svelte';

describe('SETTING_DEFS (regression guard for plugin system)', () => {
  // プラグイン機構追加で 1 件 (plugins.enabled) が増える。これが unexpected に
  // 増減したら気付けるよう、厳密な数で固定する。新しい設定追加時はここも更新する。
  // (built-in 15 件 + plugins.enabled 1 件 = 16)
  it('has exactly the expected number of built-in settings', () => {
    expect(SETTING_DEFS.length).toBe(16);
  });

  it('contains the plugins.enabled kill switch', () => {
    const def = SETTING_DEFS.find((d) => d.key === 'plugins.enabled');
    expect(def).toBeTruthy();
    expect(def?.default).toBe(true);
    expect(def?.kind).toBe('bool');
    expect(def?.section).toBe('advanced');
  });

  it('all setting keys are unique', () => {
    const keys = SETTING_DEFS.map((d) => d.key);
    expect(new Set(keys).size).toBe(keys.length);
  });

  // select の option value は常に文字列。default が数値だと
  // <select value={String(default)}> で String(1.0)==='1' となり
  // option value '1.0' と一致せず selectedIndex=-1 (空表示) になる。
  // さらに getStr/getNum 経由の消費側も型がズレてユーザ設定が無視される。
  // (regression: playback.default_rate が default:1.0 で空表示になっていた)
  it('every select setting default exactly matches one of its option values', () => {
    for (const def of SETTING_DEFS) {
      if (def.kind !== 'select') continue;
      expect(def.options, `${def.key} is select but has no options`).toBeTruthy();
      const values = (def.options ?? []).map((o) => o.value);
      // 文字列としての厳密一致を要求する (型違いの 1.0 は弾く)。
      expect(values, `${def.key} default ${JSON.stringify(def.default)} not in options`).toContain(
        def.default,
      );
    }
  });
});
