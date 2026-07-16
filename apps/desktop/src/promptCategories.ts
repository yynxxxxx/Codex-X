import * as React from "react";

import type { Lang } from "./types";

const STORAGE_KEY = "codexx.promptCategories.v1";
const STATE_VERSION = 1 as const;
export const PROMPT_CATEGORY_NAME_MAX_LENGTH = 40;

export type PromptCategory = {
  id: string;
  name: string;
};

type PromptCategoryState = {
  version: typeof STATE_VERSION;
  categories: PromptCategory[];
  defaultCategoryId: string;
  activeCategoryId: string;
  assignments: Record<string, string>;
};

function defaultCategories(lang: Lang): PromptCategory[] {
  return lang === "zh"
    ? [
        { id: "security-reverse", name: "破甲/逆向" },
        { id: "software-development", name: "软件开发" },
        { id: "writing", name: "写作辅助" },
      ]
    : [
        { id: "security-reverse", name: "Security / reverse" },
        { id: "software-development", name: "Software development" },
        { id: "writing", name: "Writing" },
      ];
}

function initialState(lang: Lang): PromptCategoryState {
  const categories = defaultCategories(lang);
  return {
    version: STATE_VERSION,
    categories,
    defaultCategoryId: categories[0].id,
    activeCategoryId: categories[0].id,
    assignments: {},
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function sanitizePromptCategoryState(value: unknown, lang: Lang): PromptCategoryState {
  const fallback = initialState(lang);
  if (!isRecord(value) || value.version !== STATE_VERSION || !Array.isArray(value.categories)) {
    return fallback;
  }

  const categories: PromptCategory[] = [];
  const seenIds = new Set<string>();
  const seenNames = new Set<string>();
  for (const candidate of value.categories) {
    if (!isRecord(candidate)) continue;
    const id = typeof candidate.id === "string" ? candidate.id.trim() : "";
    const name = typeof candidate.name === "string" ? candidate.name.trim() : "";
    const nameKey = name.toLocaleLowerCase();
    if (!id || !name || seenIds.has(id) || seenNames.has(nameKey)) continue;
    seenIds.add(id);
    seenNames.add(nameKey);
    categories.push({ id, name });
  }
  if (!categories.length) return fallback;

  const requestedDefault = typeof value.defaultCategoryId === "string" ? value.defaultCategoryId : "";
  const defaultCategoryId = seenIds.has(requestedDefault) ? requestedDefault : categories[0].id;
  const requestedActive = typeof value.activeCategoryId === "string" ? value.activeCategoryId : "";
  const activeCategoryId = seenIds.has(requestedActive) ? requestedActive : defaultCategoryId;
  const assignments: Record<string, string> = {};
  if (isRecord(value.assignments)) {
    for (const [promptKey, categoryId] of Object.entries(value.assignments)) {
      if (promptKey.trim() && typeof categoryId === "string" && seenIds.has(categoryId)) {
        assignments[promptKey] = categoryId;
      }
    }
  }

  return {
    version: STATE_VERSION,
    categories,
    defaultCategoryId,
    activeCategoryId,
    assignments,
  };
}

function loadState(lang: Lang): PromptCategoryState {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return initialState(lang);
    return sanitizePromptCategoryState(JSON.parse(raw), lang);
  } catch {
    return initialState(lang);
  }
}

function saveState(state: PromptCategoryState) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
    return true;
  } catch {
    return false;
  }
}

function nextCategoryId(categories: PromptCategory[]) {
  const used = new Set(categories.map((category) => category.id));
  const base = `category-${Date.now().toString(36)}`;
  let id = base;
  let suffix = 2;
  while (used.has(id)) {
    id = `${base}-${suffix}`;
    suffix += 1;
  }
  return id;
}

export function usePromptCategories(lang: Lang) {
  const [state, setState] = React.useState<PromptCategoryState>(() => loadState(lang));

  React.useEffect(() => {
    saveState(state);
  }, [state]);

  const categoryIds = React.useMemo(
    () => new Set(state.categories.map((category) => category.id)),
    [state.categories],
  );

  const categoryForPrompt = React.useCallback(
    (promptKey: string) => {
      const assigned = state.assignments[promptKey];
      return assigned && categoryIds.has(assigned) ? assigned : state.defaultCategoryId;
    },
    [categoryIds, state.assignments, state.defaultCategoryId],
  );

  const setActiveCategoryId = React.useCallback((categoryId: string) => {
    setState((current) => current.categories.some((category) => category.id === categoryId)
      ? { ...current, activeCategoryId: categoryId }
      : current);
  }, []);

  const addCategory = React.useCallback((name: string) => {
    const trimmed = name.trim();
    if (!trimmed
      || trimmed.length > PROMPT_CATEGORY_NAME_MAX_LENGTH
      || state.categories.some((category) => category.name.toLocaleLowerCase() === trimmed.toLocaleLowerCase())) {
      return false;
    }
    const category: PromptCategory = { id: nextCategoryId(state.categories), name: trimmed };
    setState((current) => ({
      ...current,
      categories: [...current.categories, category],
      activeCategoryId: category.id,
    }));
    return true;
  }, [state.categories]);

  const renameCategory = React.useCallback((categoryId: string, name: string) => {
    const trimmed = name.trim();
    if (!trimmed || trimmed.length > PROMPT_CATEGORY_NAME_MAX_LENGTH || state.categories.some((category) =>
      category.id !== categoryId && category.name.toLocaleLowerCase() === trimmed.toLocaleLowerCase())) {
      return false;
    }
    setState((current) => ({
      ...current,
      categories: current.categories.map((category) =>
        category.id === categoryId ? { ...category, name: trimmed } : category),
    }));
    return true;
  }, [state.categories]);

  const deleteCategory = React.useCallback((categoryId: string) => {
    if (state.categories.length <= 1 || !state.categories.some((category) => category.id === categoryId)) {
      return false;
    }
    setState((current) => {
      const categories = current.categories.filter((category) => category.id !== categoryId);
      const defaultCategoryId = current.defaultCategoryId === categoryId
        ? categories[0].id
        : current.defaultCategoryId;
      const assignments = Object.fromEntries(
        Object.entries(current.assignments).map(([promptKey, assignedCategoryId]) => [
          promptKey,
          assignedCategoryId === categoryId ? defaultCategoryId : assignedCategoryId,
        ]),
      );
      return {
        ...current,
        categories,
        defaultCategoryId,
        activeCategoryId: current.activeCategoryId === categoryId
          ? defaultCategoryId
          : current.activeCategoryId,
        assignments,
      };
    });
    return true;
  }, [state.categories]);

  const movePrompt = React.useCallback((promptKey: string, categoryId: string) => {
    if (!promptKey || !categoryIds.has(categoryId)) return;
    setState((current) => {
      const assignments = { ...current.assignments };
      if (categoryId === current.defaultCategoryId) delete assignments[promptKey];
      else assignments[promptKey] = categoryId;
      return { ...current, assignments };
    });
  }, [categoryIds]);

  const forgetPrompt = React.useCallback((promptKey: string) => {
    if (!promptKey) return;
    setState((current) => {
      if (!(promptKey in current.assignments)) return current;
      const assignments = { ...current.assignments };
      delete assignments[promptKey];
      return { ...current, assignments };
    });
  }, []);

  return {
    categories: state.categories,
    activeCategoryId: state.activeCategoryId,
    categoryForPrompt,
    setActiveCategoryId,
    addCategory,
    renameCategory,
    deleteCategory,
    movePrompt,
    forgetPrompt,
  };
}
