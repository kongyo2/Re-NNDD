// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from 'vitest';
import {
  addNgRule,
  compileRule,
  filterComments,
  importNiconicoNg,
  listNgRules,
  niconicoNgToRule,
  NICONICO_NG_NOTE,
  type NgRule,
} from './ngRules';

function rule(overrides: Partial<NgRule>): NgRule {
  return {
    id: 'r',
    targetType: 'comment_body',
    matchMode: 'partial',
    pattern: 'abc',
    scopeRanking: false,
    scopeSearch: false,
    scopeComment: true,
    enabled: true,
    createdAt: 0,
    hitCount: 0,
    ...overrides,
  };
}

describe('ignoreCase', () => {
  it('keeps the old case-sensitive behavior when unset', () => {
    const m = compileRule(rule({ pattern: 'ABC' }))!;
    expect(m('xxABCxx')).toBe(true);
    expect(m('xxabcxx')).toBe(false);
  });

  it('matches regardless of case for every mode', () => {
    expect(compileRule(rule({ pattern: 'ABC', ignoreCase: true }))!('xxabcxx')).toBe(true);
    expect(
      compileRule(rule({ matchMode: 'exact', pattern: 'Www', ignoreCase: true }))!('wWW'),
    ).toBe(true);
    expect(
      compileRule(rule({ matchMode: 'exact', pattern: 'Www', ignoreCase: true }))!('wWWw'),
    ).toBe(false);
    expect(
      compileRule(rule({ matchMode: 'regex', pattern: '^k+$', ignoreCase: true }))!('KKK'),
    ).toBe(true);
  });
});

describe('niconicoNgToRule', () => {
  it('maps word to a case-insensitive partial comment rule', () => {
    expect(niconicoNgToRule({ type: 'word', source: 'ネタバレ' })).toEqual({
      targetType: 'comment_body',
      matchMode: 'partial',
      pattern: 'ネタバレ',
      ignoreCase: true,
      scopeComment: true,
      scopeRanking: false,
      scopeSearch: false,
      enabled: true,
      note: NICONICO_NG_NOTE,
    });
  });

  it('maps id to an exact comment user rule', () => {
    expect(niconicoNgToRule({ type: 'id', source: 'nvc:AbC' })).toMatchObject({
      targetType: 'comment_user',
      matchMode: 'exact',
      pattern: 'nvc:AbC',
      scopeComment: true,
    });
  });

  it('has no local counterpart for command or unknown types', () => {
    expect(niconicoNgToRule({ type: 'command', source: 'big red' })).toBeNull();
    expect(niconicoNgToRule({ type: 'mystery', source: 'x' })).toBeNull();
    expect(niconicoNgToRule({ type: 'word', source: '  ' })).toBeNull();
  });
});

describe('importNiconicoNg', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('adds rules, skips duplicates and counts unsupported items', () => {
    addNgRule({
      targetType: 'comment_user',
      matchMode: 'exact',
      pattern: 'nvc:dup',
      scopeRanking: false,
      scopeSearch: false,
      scopeComment: true,
      enabled: true,
    });
    const result = importNiconicoNg([
      { type: 'word', source: 'SPOILER' },
      { type: 'id', source: 'nvc:dup' },
      { type: 'command', source: 'shita' },
      { type: 'word', source: 'SPOILER' },
    ]);
    expect(result).toEqual({ added: 1, duplicates: 2, unsupported: 1 });
    const rules = listNgRules();
    expect(rules).toHaveLength(2);
    const imported = rules.find((r) => r.pattern === 'SPOILER')!;
    expect(imported.note).toBe(NICONICO_NG_NOTE);

    // 取り込んだ word はニコニコと同じく大文字小文字を区別せずに効く。
    const kept = filterComments(rules, [
      { content: 'this is a spoiler!' },
      { content: 'ok', userId: 'nvc:dup' },
      { content: 'fine', userId: 'nvc:other' },
    ]);
    expect(kept).toEqual([{ content: 'fine', userId: 'nvc:other' }]);
  });

  it('does not write when nothing is added', () => {
    const result = importNiconicoNg([{ type: 'command', source: '184' }]);
    expect(result).toEqual({ added: 0, duplicates: 0, unsupported: 1 });
    expect(listNgRules()).toHaveLength(0);
  });
});
