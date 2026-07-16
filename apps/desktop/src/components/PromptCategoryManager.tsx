import { useEffect, useMemo, useState } from "react";
import {
  AlertTriangle,
  ChevronDown,
  ChevronRight,
  Edit2,
  FileText,
  Plus,
  Settings2,
  Trash2,
} from "lucide-react";

import { PROMPT_CATEGORY_NAME_MAX_LENGTH } from "../promptCategories";
import type { PromptCategory } from "../promptCategories";
import type { Lang } from "../types";
import { Button, IconButton, ModalShell, cx } from "./ui";

export type PromptCategoryItem = {
  key: string;
  title: string;
};

type PromptCategoryManagerProps = {
  open: boolean;
  lang: Lang;
  categories: PromptCategory[];
  prompts: PromptCategoryItem[];
  categoryForPrompt: (promptKey: string) => string;
  onClose: () => void;
  onAddCategory: (name: string) => boolean;
  onRenameCategory: (categoryId: string, name: string) => boolean;
  onDeleteCategory: (categoryId: string) => boolean;
  onMovePrompt: (promptKey: string, categoryId: string) => void;
};

export function PromptCategoryManager({
  open,
  lang,
  categories,
  prompts,
  categoryForPrompt,
  onClose,
  onAddCategory,
  onRenameCategory,
  onDeleteCategory,
  onMovePrompt,
}: PromptCategoryManagerProps) {
  const isChinese = lang === "zh";
  const [expandedCategories, setExpandedCategories] = useState<Set<string>>(new Set());
  const [editingCategoryId, setEditingCategoryId] = useState<string | null>(null);
  const [editingCategoryName, setEditingCategoryName] = useState("");
  const [isAddingCategory, setIsAddingCategory] = useState(false);
  const [newCategoryName, setNewCategoryName] = useState("");
  const [categoryToDelete, setCategoryToDelete] = useState<PromptCategory | null>(null);
  const [validationError, setValidationError] = useState("");

  useEffect(() => {
    if (open) return;
    setExpandedCategories(new Set());
    setEditingCategoryId(null);
    setEditingCategoryName("");
    setIsAddingCategory(false);
    setNewCategoryName("");
    setCategoryToDelete(null);
    setValidationError("");
  }, [open]);

  const promptsByCategory = useMemo(() => {
    const grouped = new Map<string, PromptCategoryItem[]>();
    for (const category of categories) grouped.set(category.id, []);
    for (const prompt of prompts) grouped.get(categoryForPrompt(prompt.key))?.push(prompt);
    return grouped;
  }, [categories, categoryForPrompt, prompts]);

  const copy = isChinese
    ? {
        title: "分类管理",
        deleteTitle: "删除提示词分类",
        deleteDescription: (name: string) => `“${name}”中的提示词会移到默认分类，提示词本身不会被删除。`,
        addCategory: "新增分类",
        namePlaceholder: "输入分类名称",
        cancel: "取消",
        save: "保存",
        add: "添加",
        confirmDelete: "确认删除",
        edit: "编辑分类",
        remove: "删除分类",
        keepOne: "至少保留一个分类",
        empty: "该分类下暂无提示词",
        move: (title: string) => `移动“${title}”到分类`,
        duplicate: `分类名称不能为空、重复或超过 ${PROMPT_CATEGORY_NAME_MAX_LENGTH} 个字符`,
        expand: (name: string) => `展开${name}`,
        collapse: (name: string) => `收起${name}`,
      }
    : {
        title: "Manage categories",
        deleteTitle: "Delete prompt category",
        deleteDescription: (name: string) => `Prompts in “${name}” will move to the default category. The prompts are not deleted.`,
        addCategory: "Add category",
        namePlaceholder: "Category name",
        cancel: "Cancel",
        save: "Save",
        add: "Add",
        confirmDelete: "Delete category",
        edit: "Edit category",
        remove: "Delete category",
        keepOne: "Keep at least one category",
        empty: "No prompts in this category",
        move: (title: string) => `Move “${title}” to category`,
        duplicate: `Category names cannot be empty, duplicated, or longer than ${PROMPT_CATEGORY_NAME_MAX_LENGTH} characters`,
        expand: (name: string) => `Expand ${name}`,
        collapse: (name: string) => `Collapse ${name}`,
      };

  const close = () => {
    if (categoryToDelete) {
      setCategoryToDelete(null);
      return;
    }
    onClose();
  };

  const toggleExpanded = (categoryId: string) => {
    setExpandedCategories((current) => {
      const next = new Set(current);
      if (next.has(categoryId)) next.delete(categoryId);
      else next.add(categoryId);
      return next;
    });
  };

  const saveNewCategory = () => {
    if (!onAddCategory(newCategoryName)) {
      setValidationError(copy.duplicate);
      return;
    }
    setIsAddingCategory(false);
    setNewCategoryName("");
    setValidationError("");
  };

  const saveCategoryName = (categoryId: string) => {
    if (!onRenameCategory(categoryId, editingCategoryName)) {
      setValidationError(copy.duplicate);
      return;
    }
    setEditingCategoryId(null);
    setEditingCategoryName("");
    setValidationError("");
  };

  const footer = categoryToDelete ? (
    <>
      <Button variant="secondary" onClick={() => setCategoryToDelete(null)} data-initial-focus>{copy.cancel}</Button>
      <Button
        variant="danger"
        icon={<Trash2 size={16} />}
        onClick={() => {
          if (onDeleteCategory(categoryToDelete.id)) setCategoryToDelete(null);
        }}
      >
        {copy.confirmDelete}
      </Button>
    </>
  ) : undefined;

  return (
    <ModalShell
      key={categoryToDelete ? "delete-category" : "manage-categories"}
      open={open}
      onClose={close}
      title={categoryToDelete ? copy.deleteTitle : (
        <span className="cx-prompt-category-modal-title"><Settings2 size={19} aria-hidden="true" />{copy.title}</span>
      )}
      description={categoryToDelete ? copy.deleteDescription(categoryToDelete.name) : undefined}
      footer={footer}
      size="md"
      closeLabel={isChinese ? "关闭" : "Close"}
      className={cx("cx-prompt-category-modal", categoryToDelete && "cx-prompt-category-modal--confirm")}
      bodyClassName="cx-prompt-category-modal-body"
    >
      {categoryToDelete ? (
        <div className="cx-prompt-category-delete-warning">
          <AlertTriangle size={22} aria-hidden="true" />
          <strong>{categoryToDelete.name}</strong>
        </div>
      ) : (
        <>
          <div className="cx-prompt-category-list">
            {categories.map((category) => {
              const categoryPrompts = promptsByCategory.get(category.id) || [];
              const expanded = expandedCategories.has(category.id);
              const editing = editingCategoryId === category.id;
              return (
                <section className="cx-prompt-category-item" key={category.id}>
                  <div className="cx-prompt-category-row">
                    {editing ? (
                      <div className="cx-prompt-category-editor">
                        <input
                          value={editingCategoryName}
                          onChange={(event) => {
                            setEditingCategoryName(event.currentTarget.value);
                            setValidationError("");
                          }}
                          onKeyDown={(event) => {
                            if (event.key === "Enter") saveCategoryName(category.id);
                            if (event.key === "Escape") {
                              setEditingCategoryId(null);
                              setValidationError("");
                            }
                          }}
                          aria-label={copy.edit}
                          maxLength={PROMPT_CATEGORY_NAME_MAX_LENGTH}
                          autoFocus
                        />
                        <button type="button" onClick={() => {
                          setEditingCategoryId(null);
                          setValidationError("");
                        }}>{copy.cancel}</button>
                        <button type="button" onClick={() => saveCategoryName(category.id)} disabled={!editingCategoryName.trim()}>{copy.save}</button>
                      </div>
                    ) : (
                      <>
                        <button
                          type="button"
                          className="cx-prompt-category-expand"
                          onClick={() => toggleExpanded(category.id)}
                          aria-expanded={expanded}
                          aria-label={expanded ? copy.collapse(category.name) : copy.expand(category.name)}
                        >
                          {expanded ? <ChevronDown size={16} /> : <ChevronRight size={16} />}
                          <span>{category.name}</span>
                          <small>{categoryPrompts.length}</small>
                        </button>
                        <div className="cx-prompt-category-actions">
                          <IconButton
                            icon={<Edit2 size={15} />}
                            label={copy.edit}
                            variant="ghost"
                            size="sm"
                            onClick={() => {
                              setEditingCategoryId(category.id);
                              setEditingCategoryName(category.name);
                              setIsAddingCategory(false);
                              setValidationError("");
                            }}
                          />
                          <IconButton
                            icon={<Trash2 size={15} />}
                            label={categories.length <= 1 ? copy.keepOne : copy.remove}
                            variant="ghost"
                            size="sm"
                            disabled={categories.length <= 1}
                            onClick={() => setCategoryToDelete(category)}
                          />
                        </div>
                      </>
                    )}
                  </div>
                  {expanded && (
                    <div className="cx-prompt-category-prompts">
                      {categoryPrompts.length ? categoryPrompts.map((prompt) => (
                        <div className="cx-prompt-category-prompt" key={prompt.key}>
                          <FileText size={14} aria-hidden="true" />
                          <span title={prompt.title}>{prompt.title}</span>
                          <select
                            value={category.id}
                            onChange={(event) => onMovePrompt(prompt.key, event.currentTarget.value)}
                            aria-label={copy.move(prompt.title)}
                          >
                            {categories.map((option) => <option key={option.id} value={option.id}>{option.name}</option>)}
                          </select>
                        </div>
                      )) : <p className="cx-prompt-category-empty">{copy.empty}</p>}
                    </div>
                  )}
                </section>
              );
            })}
            {isAddingCategory && (
              <div className="cx-prompt-category-new">
                <input
                  value={newCategoryName}
                  onChange={(event) => {
                    setNewCategoryName(event.currentTarget.value);
                    setValidationError("");
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") saveNewCategory();
                    if (event.key === "Escape") {
                      setIsAddingCategory(false);
                      setValidationError("");
                    }
                  }}
                  placeholder={copy.namePlaceholder}
                  maxLength={PROMPT_CATEGORY_NAME_MAX_LENGTH}
                  autoFocus
                />
                <button type="button" onClick={() => {
                  setIsAddingCategory(false);
                  setValidationError("");
                }}>{copy.cancel}</button>
                <button type="button" onClick={saveNewCategory} disabled={!newCategoryName.trim()}>{copy.add}</button>
              </div>
            )}
          </div>
          {validationError && <p className="cx-prompt-category-error" role="alert">{validationError}</p>}
          {!isAddingCategory && (
            <button
              type="button"
              className="cx-prompt-category-add"
              onClick={() => {
                setIsAddingCategory(true);
                setEditingCategoryId(null);
                setNewCategoryName("");
                setValidationError("");
              }}
            >
              <Plus size={16} aria-hidden="true" />{copy.addCategory}
            </button>
          )}
        </>
      )}
    </ModalShell>
  );
}
