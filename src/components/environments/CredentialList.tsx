import { useState } from "react";
import { Star, PencilSimple, Trash, Plugs } from "@phosphor-icons/react";
import type { EnvironmentAuthType, TestConnectionResult } from "@/lib/types";
import type { StagedCredential } from "./staged";

const inputCls =
  "w-full bg-muted border border-border rounded-md text-sm text-foreground px-3 py-1.5 placeholder:text-muted-foreground/50 outline-none";

const selectCls =
  "w-28 shrink-0 bg-muted border border-border rounded-md text-sm text-foreground px-2 py-1.5 outline-none cursor-pointer";

interface CredentialListProps {
  staged: StagedCredential[];
  testingId: string | null;
  testResults: Record<string, TestConnectionResult | undefined>;
  onSetDefault: (key: string) => void;
  onRemove: (key: string) => void;
  onEditSave: (
    key: string,
    username: string,
    authType: EnvironmentAuthType,
    privateKeyPath: string,
    secret: string,
  ) => void;
  onTest: (key: string) => void;
  onAdd: (
    username: string,
    authType: EnvironmentAuthType,
    privateKeyPath: string,
    secret: string,
    makeDefault: boolean,
  ) => boolean;
}

export function CredentialList(props: CredentialListProps) {
  const [addForm, setAddForm] = useState({
    username: "",
    authType: "password" as EnvironmentAuthType,
    privateKeyPath: "",
    secret: "",
    makeDefault: false,
  });
  const [editing, setEditing] = useState<{
    key: string;
    username: string;
    authType: EnvironmentAuthType;
    privateKeyPath: string;
    secret: string;
  } | null>(null);

  const handleAdd = () => {
    const ok = props.onAdd(
      addForm.username.trim(),
      addForm.authType,
      addForm.privateKeyPath.trim(),
      addForm.secret,
      addForm.makeDefault,
    );
    if (ok) {
      setAddForm({ username: "", authType: "password", privateKeyPath: "", secret: "", makeDefault: false });
    }
  };

  return (
    <div className="space-y-2">
      <ul className="space-y-1">
        {props.staged.map((c) => {
          const result = props.testResults[c.key];
          return (
            <li
              key={c.key}
              className="text-xs px-3 py-1.5 rounded-md border border-border bg-surface-2 space-y-1"
            >
              <div className="flex items-center gap-2">
                <button
                  onClick={() => props.onSetDefault(c.key)}
                  aria-label={c.isDefault ? "当前默认凭证" : "设为默认"}
                  className="cursor-pointer"
                  title={c.isDefault ? "默认登录用户" : "设为默认登录用户"}
                >
                  <Star
                    size={14}
                    weight={c.isDefault ? "fill" : "regular"}
                    className={c.isDefault ? "text-accent" : "text-muted-foreground"}
                    aria-hidden="true"
                  />
                </button>
                <span className="font-mono">{c.username}</span>
                <span className="text-muted-foreground">
                  {c.authType === "private_key" ? "私钥" : "密码"}
                </span>
                {c.isDefault && (
                  <span className="px-1.5 py-0.5 rounded bg-accent/15 text-accent text-[10px]">默认</span>
                )}
                <span className="flex-1" />
                <button
                  onClick={() => props.onTest(c.key)}
                  disabled={props.testingId === c.key}
                  className="text-muted-foreground hover:text-foreground cursor-pointer disabled:opacity-50"
                  title="测试此凭证"
                >
                  <Plugs size={13} aria-hidden="true" />
                </button>
                <button
                  onClick={() => {
                    if (editing?.key === c.key) {
                      setEditing(null);
                    } else {
                      setEditing({
                        key: c.key,
                        username: c.username,
                        authType: c.authType,
                        privateKeyPath: c.privateKeyPath ?? "",
                        secret: "",
                      });
                    }
                  }}
                  className="text-muted-foreground hover:text-foreground cursor-pointer"
                  title="编辑"
                >
                  <PencilSimple size={13} aria-hidden="true" />
                </button>
                <button
                  onClick={() => props.onRemove(c.key)}
                  className="text-muted-foreground hover:text-destructive cursor-pointer"
                  title="移除"
                >
                  <Trash size={13} aria-hidden="true" />
                </button>
              </div>
              {result && (
                <div className={result.ok ? "text-success" : "text-destructive"}>
                  {result.ok ? `连接成功（${result.latency_ms}ms）` : `连接失败：${result.error}`}
                </div>
              )}
              {editing?.key === c.key && (
                <div className="space-y-1.5 pt-1 border-t border-border">
                  <div className="flex gap-2">
                    <input
                      className={inputCls}
                      placeholder="用户名"
                      value={editing.username}
                      onChange={(e) => setEditing({ ...editing, username: e.target.value })}
                      aria-label="凭证用户名"
                    />
                    <select
                      className={selectCls}
                      value={editing.authType}
                      onChange={(e) =>
                        setEditing({ ...editing, authType: e.target.value as EnvironmentAuthType })
                      }
                      aria-label="凭证认证方式"
                    >
                      <option value="password">密码</option>
                      <option value="private_key">私钥</option>
                    </select>
                  </div>
                  {editing.authType === "private_key" && (
                    <input
                      className={inputCls}
                      placeholder="私钥路径（~/.ssh/...）"
                      value={editing.privateKeyPath}
                      onChange={(e) => setEditing({ ...editing, privateKeyPath: e.target.value })}
                      aria-label="凭证私钥路径"
                      style={{ fontFamily: "var(--font-mono)" }}
                    />
                  )}
                  <input
                    type="password"
                    className={inputCls}
                    placeholder={
                      editing.authType === "private_key" ? "私钥口令（留空 = 不修改）" : "密码（留空 = 不修改）"
                    }
                    value={editing.secret}
                    onChange={(e) => setEditing({ ...editing, secret: e.target.value })}
                    aria-label="凭证密钥"
                  />
                  <div className="flex gap-2 justify-end">
                    <button
                      className="px-2 py-1 rounded-md border border-border bg-surface-2 hover:bg-surface-3 cursor-pointer"
                      onClick={() => setEditing(null)}
                    >
                      取消
                    </button>
                    <button
                      className="px-2 py-1 rounded-md bg-accent text-accent-foreground hover:bg-accent/80 cursor-pointer"
                      onClick={() => {
                        props.onEditSave(
                          c.key,
                          editing.username.trim(),
                          editing.authType,
                          editing.privateKeyPath.trim(),
                          editing.secret,
                        );
                        setEditing(null);
                      }}
                    >
                      保存
                    </button>
                  </div>
                </div>
              )}
            </li>
          );
        })}
      </ul>

      <div className="space-y-1.5">
        <div className="flex gap-2">
          <input
            className={`${inputCls} flex-1`}
            placeholder="用户名（如 svcapp）"
            value={addForm.username}
            onChange={(e) => setAddForm({ ...addForm, username: e.target.value })}
            aria-label="新凭证用户名"
          />
          <select
            className={selectCls}
            value={addForm.authType}
            onChange={(e) => setAddForm({ ...addForm, authType: e.target.value as EnvironmentAuthType })}
            aria-label="新凭证认证方式"
          >
            <option value="password">密码</option>
            <option value="private_key">私钥</option>
          </select>
        </div>
        {addForm.authType === "private_key" && (
          <input
            className={inputCls}
            placeholder="私钥路径（~/.ssh/...）"
            value={addForm.privateKeyPath}
            onChange={(e) => setAddForm({ ...addForm, privateKeyPath: e.target.value })}
            aria-label="新凭证私钥路径"
            style={{ fontFamily: "var(--font-mono)" }}
          />
        )}
        <div className="flex gap-2 items-center">
          <input
            type="password"
            className={`${inputCls} flex-1`}
            placeholder={addForm.authType === "private_key" ? "私钥口令（可选）" : "密码"}
            value={addForm.secret}
            onChange={(e) => setAddForm({ ...addForm, secret: e.target.value })}
            aria-label="新凭证密钥"
          />
          <label className="flex items-center gap-1 text-xs text-muted-foreground whitespace-nowrap">
            <input
              type="checkbox"
              checked={addForm.makeDefault}
              onChange={(e) => setAddForm({ ...addForm, makeDefault: e.target.checked })}
            />
            设为默认
          </label>
          <button
            className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs hover:bg-surface-3 cursor-pointer whitespace-nowrap"
            onClick={handleAdd}
          >
            添加凭证
          </button>
        </div>
      </div>
    </div>
  );
}
