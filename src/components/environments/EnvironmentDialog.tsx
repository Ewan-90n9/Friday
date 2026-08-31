import { useEffect, useRef, useState } from "react";
import { X, CircleNotch, Plug, CheckCircle, XCircle } from "@phosphor-icons/react";
import type { EnvironmentRow, EnvCredentialRow, TestConnectionResult } from "@/lib/types";
import { listEnvCredentials, addEnvCredential, deleteEnvCredential, setDefaultEnvCredential } from "@/lib/ipc";
import { useEnvStore } from "@/store/envStore";

interface EnvironmentDialogProps {
  open: boolean;
  onClose: () => void;
  editing: EnvironmentRow | null; // null = 新增
}

const EMPTY_FORM = {
  name: "",
  host: "",
  port: "22",
  user: "root",
  authType: "private_key" as "private_key" | "password",
  privateKeyPath: "",
  password: "",
};

function guessDefaultKeyPath(): string {
  return "~/.ssh/id_ed25519";
}

export function EnvironmentDialog({ open, onClose, editing }: EnvironmentDialogProps) {
  const add = useEnvStore((s) => s.add);
  const update = useEnvStore((s) => s.update);
  const test = useEnvStore((s) => s.test);
  const storeError = useEnvStore((s) => s.error);

  const dialogRef = useRef<HTMLDialogElement>(null);
  const [form, setForm] = useState(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<TestConnectionResult | null>(null);
  const [formError, setFormError] = useState<string | null>(null);

  const [creds, setCreds] = useState<EnvCredentialRow[]>([]);
  const [credForm, setCredForm] = useState({
    username: "",
    authType: "password" as "private_key" | "password",
    privateKeyPath: "",
    password: "",
    makeDefault: false,
  });
  const [credError, setCredError] = useState<string | null>(null);
  const [credBusy, setCredBusy] = useState(false);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open) {
      setForm(
        editing
          ? {
              name: editing.name,
              host: editing.host,
              port: String(editing.port),
              user: editing.user,
              authType: editing.auth_type,
              privateKeyPath: editing.private_key_path ?? "",
              password: "",
            }
          : { ...EMPTY_FORM, privateKeyPath: guessDefaultKeyPath() },
      );
      setTestResult(null);
      setFormError(null);
      setCreds([]);
      setCredError(null);
      setCredForm({ username: "", authType: "password", privateKeyPath: "", password: "", makeDefault: false });
      if (editing) {
        listEnvCredentials(editing.id).then(setCreds).catch(() => setCreds([]));
      }
      if (!dialog.open) dialog.showModal();
    } else if (dialog.open) {
      dialog.close();
    }
  }, [open, editing]);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const handleClose = () => onClose();
    dialog.addEventListener("close", handleClose);
    return () => dialog.removeEventListener("close", handleClose);
  }, [onClose]);

  const handleSave = async () => {
    if (!form.name.trim() || !form.host.trim() || !form.user.trim()) {
      setFormError("名称 / 主机 / 用户名不能为空");
      return;
    }
    if (form.authType === "private_key" && !form.privateKeyPath.trim()) {
      setFormError("私钥认证需要填写私钥路径");
      return;
    }
    const port = parsePort(form.port);
    if (port === null) {
      setFormError("端口必须是 1-65535 的数字");
      return;
    }
    setSaving(true);
    setFormError(null);
    try {
      const params = {
        name: form.name.trim(),
        host: form.host.trim(),
        port,
        user: form.user.trim(),
        authType: form.authType,
        privateKeyPath: form.authType === "private_key" ? form.privateKeyPath.trim() : null,
        password: form.password ? form.password : null,
      };
      const ok = editing ? await update({ id: editing.id, ...params }) : await add(params);
      if (ok) onClose();
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async () => {
    if (!form.host.trim() || !form.user.trim()) {
      setFormError("主机 / 用户名不能为空");
      return;
    }
    const port = parsePort(form.port);
    if (port === null) {
      setFormError("端口必须是 1-65535 的数字");
      return;
    }
    setTesting(true);
    setTestResult(null);
    setFormError(null);
    try {
      setTestResult(
        await test({
          environmentId: editing?.id ?? null,
          host: form.host.trim(),
          port,
          user: form.user.trim(),
          authType: form.authType,
          privateKeyPath: form.authType === "private_key" ? form.privateKeyPath.trim() : null,
          password: form.password ? form.password : null,
        }),
      );
    } finally {
      setTesting(false);
    }
  };

  const handleAddCred = async () => {
    if (!editing) return;
    if (!credForm.username.trim()) {
      setCredError("用户名不能为空");
      return;
    }
    if (credForm.authType === "private_key" && !credForm.privateKeyPath.trim()) {
      setCredError("私钥认证需要填写私钥路径");
      return;
    }
    setCredBusy(true);
    setCredError(null);
    try {
      await addEnvCredential({
        environmentId: editing.id,
        username: credForm.username.trim(),
        authType: credForm.authType,
        privateKeyPath: credForm.authType === "private_key" ? credForm.privateKeyPath.trim() : null,
        password: credForm.password || null,
        makeDefault: credForm.makeDefault,
      });
      setCreds(await listEnvCredentials(editing.id));
      setCredForm({ username: "", authType: "password", privateKeyPath: "", password: "", makeDefault: false });
    } catch (e) {
      setCredError(String(e));
    } finally {
      setCredBusy(false);
    }
  };

  const handleDeleteCred = async (cred: EnvCredentialRow) => {
    if (!editing) return;
    try {
      await deleteEnvCredential(editing.id, cred.id);
      setCreds(await listEnvCredentials(editing.id));
    } catch (e) {
      setCredError(String(e));
    }
  };

  const handleSetDefaultCred = async (cred: EnvCredentialRow) => {
    if (!editing) return;
    try {
      await setDefaultEnvCredential(editing.id, cred.id);
      setCreds(await listEnvCredentials(editing.id));
    } catch (e) {
      setCredError(String(e));
    }
  };

  return (
    <dialog
      ref={dialogRef}
      aria-label={editing ? "编辑环境" : "新增环境"}
      className="z-50 w-[480px] max-w-[90vw] rounded-xl bg-card border border-border p-0 text-foreground overflow-hidden"
    >
      <div className="flex flex-col max-h-[85vh] overflow-hidden rounded-xl">
        <div className="flex items-center justify-between px-5 py-4 border-b border-border shrink-0">
          <h2 className="text-sm font-medium">{editing ? "编辑环境" : "新增环境"}</h2>
          <button
            onClick={onClose}
            aria-label="关闭"
            className="flex items-center justify-center w-7 h-7 rounded-md text-muted-foreground hover:text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
          >
            <X size={16} aria-hidden="true" />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto px-5 py-4 space-y-3 min-h-0">
          <Field label="名称" htmlFor="env-name">
            <input
              id="env-name"
              type="text"
              value={form.name}
              onChange={(e) => setForm({ ...form, name: e.target.value })}
              placeholder="prod-jvm-01"
              className={inputCls}
            />
          </Field>
          <div className="flex gap-3">
            <Field label="主机" htmlFor="env-host" className="flex-1">
              <input
                id="env-host"
                type="text"
                value={form.host}
                onChange={(e) => setForm({ ...form, host: e.target.value })}
                placeholder="10.0.0.1"
                className={inputCls}
              />
            </Field>
            <Field label="端口" htmlFor="env-port" className="w-24">
              <input
                id="env-port"
                type="text"
                inputMode="numeric"
                value={form.port}
                onChange={(e) => setForm({ ...form, port: e.target.value })}
                className={inputCls}
              />
            </Field>
          </div>
          <Field label="用户名" htmlFor="env-user">
            <input
              id="env-user"
              type="text"
              value={form.user}
              onChange={(e) => setForm({ ...form, user: e.target.value })}
              placeholder="root"
              className={inputCls}
            />
          </Field>
          <Field label="认证方式" htmlFor="env-auth-type">
            <select
              id="env-auth-type"
              value={form.authType}
              onChange={(e) =>
                setForm({ ...form, authType: e.target.value as "private_key" | "password" })
              }
              className={`${inputCls} cursor-pointer`}
            >
              <option value="private_key">私钥（推荐）</option>
              <option value="password">密码</option>
            </select>
          </Field>
          {form.authType === "private_key" ? (
            <Field label="私钥路径" htmlFor="env-private-key">
              <input
                id="env-private-key"
                type="text"
                value={form.privateKeyPath}
                onChange={(e) => setForm({ ...form, privateKeyPath: e.target.value })}
                placeholder="~/.ssh/id_ed25519"
                className={inputCls}
                style={{ fontFamily: "var(--font-mono)" }}
              />
              <p className="text-xs text-muted-foreground mt-1">
                引用本机 ~/.ssh/ 下的私钥文件，不复制。带 passphrase 时请用下方密钥字段保存。
              </p>
            </Field>
          ) : null}
          <Field
            label={form.authType === "private_key" ? "密钥口令（可选）" : "密码"}
            htmlFor="env-password"
          >
            <input
              id="env-password"
              type="password"
              value={form.password}
              onChange={(e) => setForm({ ...form, password: e.target.value })}
              placeholder={editing ? (form.authType !== editing.auth_type ? "切换认证方式将清除已存密钥" : "留空表示不修改") : ""}
              className={inputCls}
            />
            <p className="text-xs text-muted-foreground mt-1">
              存入操作系统密钥链（Windows 凭据管理器），不写入数据库。
            </p>
          </Field>

          {editing && (
            <div className="pt-2 border-t border-border space-y-2">
              <p className="text-xs text-muted-foreground">
                多用户凭证：目标 JVM 以其他用户运行时（arthas attach 需要同用户），为该环境录入对应用户的
                SSH 凭证。默认凭证即日常连接使用的用户。
              </p>
              {creds.length > 0 && (
                <ul className="space-y-1">
                  {creds.map((cred) => (
                    <li
                      key={cred.id}
                      className="flex items-center gap-2 text-xs px-3 py-1.5 rounded-md border border-border bg-surface-2"
                    >
                      <span className="font-mono">{cred.username}</span>
                      <span className="text-muted-foreground">
                        {cred.auth_type === "private_key" ? "私钥" : "密码"}
                      </span>
                      {cred.is_default && (
                        <span className="px-1.5 py-0.5 rounded bg-accent/15 text-accent text-[10px]">默认</span>
                      )}
                      <span className="flex-1" />
                      {!cred.is_default && (
                        <button
                          onClick={() => handleSetDefaultCred(cred)}
                          className="text-muted-foreground hover:text-foreground cursor-pointer"
                        >
                          设为默认
                        </button>
                      )}
                      {!cred.is_default && (
                        <button
                          onClick={() => handleDeleteCred(cred)}
                          className="text-muted-foreground hover:text-destructive cursor-pointer"
                        >
                          删除
                        </button>
                      )}
                    </li>
                  ))}
                </ul>
              )}
              <div className="flex gap-2">
                <input
                  type="text"
                  aria-label="凭证用户名"
                  placeholder="用户名（如 svcapp）"
                  value={credForm.username}
                  onChange={(e) => setCredForm({ ...credForm, username: e.target.value })}
                  className={`${inputCls} flex-1`}
                />
                <select
                  aria-label="凭证认证方式"
                  value={credForm.authType}
                  onChange={(e) =>
                    setCredForm({ ...credForm, authType: e.target.value as "private_key" | "password" })
                  }
                  className={`${inputCls} w-28 cursor-pointer`}
                >
                  <option value="password">密码</option>
                  <option value="private_key">私钥</option>
                </select>
              </div>
              {credForm.authType === "private_key" && (
                <input
                  type="text"
                  aria-label="凭证私钥路径"
                  placeholder="私钥路径（~/.ssh/...）"
                  value={credForm.privateKeyPath}
                  onChange={(e) => setCredForm({ ...credForm, privateKeyPath: e.target.value })}
                  className={inputCls}
                  style={{ fontFamily: "var(--font-mono)" }}
                />
              )}
              <div className="flex gap-2 items-center">
                <input
                  type="password"
                  aria-label="凭证密钥"
                  placeholder={credForm.authType === "private_key" ? "私钥口令（可选）" : "密码"}
                  value={credForm.password}
                  onChange={(e) => setCredForm({ ...credForm, password: e.target.value })}
                  className={`${inputCls} flex-1`}
                />
                <label className="flex items-center gap-1 text-xs text-muted-foreground whitespace-nowrap">
                  <input
                    type="checkbox"
                    checked={credForm.makeDefault}
                    onChange={(e) => setCredForm({ ...credForm, makeDefault: e.target.checked })}
                  />
                  设为默认
                </label>
                <button
                  onClick={handleAddCred}
                  disabled={credBusy}
                  className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs hover:bg-surface-3 transition-colors cursor-pointer disabled:opacity-50 whitespace-nowrap"
                >
                  添加凭证
                </button>
              </div>
              {credError && <p className="text-xs text-destructive break-words">{credError}</p>}
            </div>
          )}

          {testResult && (
            <div
              className={`flex items-center gap-2 text-xs px-3 py-2 rounded-md border ${
                testResult.ok
                  ? "bg-success/10 text-success border-success/20"
                  : "bg-destructive/10 text-destructive border-destructive/20"
              }`}
            >
              {testResult.ok ? (
                <CheckCircle size={14} weight="fill" aria-hidden="true" />
              ) : (
                <XCircle size={14} weight="fill" aria-hidden="true" />
              )}
              {testResult.ok
                ? `连接成功（${testResult.latency_ms}ms）`
                : `连接失败：${testResult.error}`}
            </div>
          )}

          {(formError ?? storeError) && (
            <p role="alert" className="text-xs text-destructive break-words">
              {formError ?? storeError}
            </p>
          )}
        </div>

        <div className="flex items-center gap-2 px-5 py-4 border-t border-border shrink-0">
          <button
            onClick={handleTest}
            disabled={testing}
            className="flex items-center gap-2 px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs text-foreground hover:bg-surface-3 transition-colors cursor-pointer disabled:opacity-50"
          >
            {testing ? (
              <CircleNotch size={14} className="animate-spin" aria-hidden="true" />
            ) : (
              <Plug size={14} aria-hidden="true" />
            )}
            测试连接
          </button>
          <div className="flex-1" />
          <button
            onClick={onClose}
            className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
          >
            取消
          </button>
          <button
            onClick={handleSave}
            disabled={saving}
            className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-accent text-accent-foreground text-xs hover:bg-accent/80 transition-colors cursor-pointer disabled:opacity-50"
          >
            {saving && <CircleNotch size={14} className="animate-spin" aria-hidden="true" />}
            保存
          </button>
        </div>
      </div>
    </dialog>
  );
}

const inputCls =
  "w-full bg-muted border border-border rounded-md text-sm text-foreground px-3 py-1.5 placeholder:text-muted-foreground/50 outline-none";

/** 端口解析：1-65535 的整数，非法返回 null（不再静默回退 22） */
function parsePort(raw: string): number | null {
  const n = parseInt(raw, 10);
  if (!Number.isInteger(n) || n < 1 || n > 65535 || String(n) !== raw.trim()) return null;
  return n;
}

function Field({
  label,
  htmlFor,
  className = "",
  children,
}: {
  label: string;
  htmlFor: string;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <div className={className}>
      <label htmlFor={htmlFor} className="block text-xs text-muted-foreground mb-1">
        {label}
      </label>
      {children}
    </div>
  );
}
