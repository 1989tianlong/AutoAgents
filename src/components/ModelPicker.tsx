import { useEffect, useRef, useState, type ReactNode } from "react";
import type { ModelOption } from "../lib/types";

interface Props {
  options: ModelOption[];
  /** 用户填写过、清单之外的模型 */
  custom: string[];
  /** 不选模型时 Agent 实际使用的默认值，仅用于提示 */
  defaultModel: string | null;
  /** 选中的模型 ID；空串表示沿用默认 */
  value: string;
  onChange: (model: string) => void;
  /** 加在外层宽度上的类名（默认 max-w-md；嵌在窄行里时可换成 flex-1 之类） */
  className?: string;
  /** 透传给输入框的 testid */
  testid?: string;
}

/** 输入框里显示的文字：清单里的模型显示名称，自定义模型显示 ID 本身 */
function displayText(value: string, options: ModelOption[]): string {
  return options.find((o) => o.value === value)?.label ?? value;
}

/** 名称 / ID / 说明里包含关键字即匹配 */
function matches(option: ModelOption, keyword: string): boolean {
  return [option.label, option.value, option.detail ?? ""].some((s) =>
    s.toLowerCase().includes(keyword),
  );
}

/**
 * 模型选择：可以输入关键字在清单里搜索，也可以直接填写清单之外的模型 ID
 * （经由网关接入的自定义模型），按回车或失焦即生效。
 */
export default function ModelPicker({ options, custom, defaultModel, value, onChange, className, testid }: Props) {
  const all = [
    ...options,
    ...custom
      .filter((m) => !options.some((o) => o.value === m))
      .map((m): ModelOption => ({ value: m, label: m, detail: "自定义" })),
  ];
  const [text, setText] = useState(() => displayText(value, all));
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const selectedText = displayText(value, all);
  // 外部重置（切换 Agent）或清单加载完成后，同步输入框
  useEffect(() => {
    setText((t) => (t === selectedText ? t : selectedText));
  }, [selectedText]);

  // 输入的是搜索关键字（而不是所选模型的名称）时，按关键字过滤清单。
  // 注意自定义模型的名称就是 ID 本身，不能只比较文字是否变化。
  const inList = all.some((o) => o.value === value);
  const searching = text.trim() !== "" && !(inList && text === selectedText);
  const keyword = text.trim().toLowerCase();
  const shown = searching ? all.filter((o) => matches(o, keyword)) : all;
  // 列表项：空串表示「默认」
  const items: string[] = ["", ...shown.map((o) => o.value)];

  const edit = (raw: string) => {
    setText(raw);
    // 输入后高亮第一个匹配项，回车即选中；没有匹配项则留在「默认」
    const key = raw.trim().toLowerCase();
    setActive(key !== "" && all.some((o) => matches(o, key)) ? 1 : 0);
    // 与某个模型的名称或 ID 完全一致才算选中它，否则按自定义模型处理
    const hit = all.find((o) => o.value === raw.trim() || o.label === raw);
    onChange(hit ? hit.value : raw);
  };

  const pick = (model: string) => {
    onChange(model);
    setText(model === "" ? "" : displayText(model, all));
    setOpen(false);
    inputRef.current?.blur();
  };

  const confirm = () => {
    if (active >= 1) {
      pick(items[active] ?? "");
    } else {
      // 没有高亮项：回车只是收起清单，保留当前输入（可能是自定义模型 ID）
      setOpen(false);
      inputRef.current?.blur();
    }
  };

  return (
    <div className={`relative w-full ${className ?? "max-w-md"}`}>
      <input
        ref={inputRef}
        value={text}
        data-testid={testid}
        onChange={(e) => {
          edit(e.currentTarget.value);
          setOpen(true);
        }}
        onFocus={() => setOpen(true)}
        onBlur={() => setOpen(false)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setOpen(true);
            setActive((a) => Math.min(a + 1, items.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setActive((a) => Math.max(a - 1, 1));
          } else if (e.key === "Enter") {
            e.preventDefault();
            confirm();
          } else if (e.key === "Escape") {
            setOpen(false);
          }
        }}
        placeholder={`默认（${defaultModel ?? "Agent 自身配置"}）`}
        spellCheck={false}
        autoComplete="off"
        className="w-full rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 placeholder:text-slate-600 focus:border-sky-600 focus:outline-none"
      />

      {open && (
        <div className="absolute z-10 mt-1 max-h-64 w-full overflow-y-auto rounded-lg border border-slate-700 bg-slate-900 py-1 shadow-xl shadow-black/50">
          <Item active={active === 0} onPick={() => pick("")}>
            <span className="text-slate-300">默认</span>
            <span className="ml-2 truncate font-mono text-[11px] text-slate-500">
              {defaultModel ?? "Agent 自身配置"}
            </span>
          </Item>
          {shown.map((o, i) => (
            <Item key={o.value} active={active === i + 1} onPick={() => pick(o.value)}>
              <span className="truncate text-slate-200">{o.label}</span>
              <span className="ml-2 shrink-0 text-[11px] text-slate-500">{o.detail}</span>
            </Item>
          ))}
          {searching && shown.length === 0 && (
            <div className="px-3 py-2 text-xs leading-5 text-slate-500">
              清单里没有匹配项，将作为自定义模型使用：
              <span className="ml-1 break-all font-mono text-slate-300">{text.trim()}</span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function Item({
  active,
  onPick,
  children,
}: {
  active: boolean;
  onPick: () => void;
  children: ReactNode;
}) {
  return (
    // 鼠标：用 onMouseDown 抢在输入框失焦之前选中；
    // 键盘回车 / 辅助功能触发的是 click，也要响应
    <button
      type="button"
      onMouseDown={(e) => {
        e.preventDefault();
        onPick();
      }}
      onClick={onPick}
      className={`flex w-full items-baseline gap-1 px-3 py-1.5 text-left text-xs ${
        active ? "bg-sky-950/60 text-white" : "hover:bg-slate-800"
      }`}
    >
      {children}
    </button>
  );
}
