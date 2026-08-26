import { useEffect, useRef, useState } from "react";
import { X, CircleNotch, Plug, CheckCircle, XCircle } from "@phosphor-icons/react";
import type { EnvironmentRow, TestConnectionResult } from "@/lib/types";
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
  const [savedEnvId, setSavedEnvId] = useState<string | null>(null);
  const [formError, setFormError] = useState<string | null>(null);

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
      setSavedEnvId(editing?.id ?? null);
      setFormError(null);
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
    setSaving(true);
    setFormError(null);
    try {
      const params = {
        name: form.name.trim(),
        host: form.host.trim(),
        port: parseInt(form.port, 10) || 22,
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
    if (!savedEnvId) {
      setFormError("请先保存环境再测试连接");
      return;
    }
    setTesting(true);
    setTestResult(null);
    try {
      setTestResult(await test(savedEnvId));
    } finally {
      setTesting(false);
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
          <Field label="名称">
            <input
              type="text"
              value={form.name}
              onChange={(e) => setForm({ ...form, name: e.target.value })}
              placeholder="prod-jvm-01"
              className={inputCls}
            />
          </Field>
          <div className="flex gap-3">
            <Field label="主机" className="flex-1">
              <input
                type="text"
                value={form.host}
                onChange={(e) => setForm({ ...form, host: e.target.value })}
                placeholder="10.0.0.1"
                className={inputCls}
              />
            </Field>
            <Field label="端口" className="w-24">
              <input
                type="text"
                value={form.port}
                onChange={(e) => setForm({ ...form, port: e.target.value })}
                className={inputCls}
              />
            </Field>
          </div>
          <Field label="用户名">
            <input
              type="text"
              value={form.user}
              onChange={(e) => setForm({ ...form, user: e.target.value })}
              placeholder="root"
              className={inputCls}
            />
          </Field>
          <Field label="认证方式">
            <select
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
            <Field label="私钥路径">
              <input
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
          <Field label={form.authType === "private_key" ? "密钥口令（可选）" : "密码"}>
            <input
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
            <p className="text-xs text-destructive break-words">{formError ?? storeError}</p>
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

function Field({
  label,
  className = "",
  children,
}: {
  label: string;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <div className={className}>
      <label className="block text-xs text-muted-foreground mb-1">{label}</label>
      {children}
    </div>
  );
}
