import { useEffect, useRef, useState } from "react";
import { X, CircleNotch } from "@phosphor-icons/react";
import type { EnvironmentRow, TestConnectionResult } from "@/lib/types";
import { listEnvCredentials } from "@/lib/ipc";
import { useEnvStore } from "@/store/envStore";
import { CredentialList } from "./CredentialList";
import { DiscardChangesDialog } from "./DiscardChangesDialog";
import { fromStored, toInput, addStaged, type StagedCredential } from "./staged";

interface EnvironmentDialogProps {
  open: boolean;
  onClose: () => void;
  editing: EnvironmentRow | null;
}

const EMPTY_FORM = { name: "", host: "", port: "22" };

export function EnvironmentDialog({ open, onClose, editing }: EnvironmentDialogProps) {
  const save = useEnvStore((s) => s.save);
  const test = useEnvStore((s) => s.test);
  const storeError = useEnvStore((s) => s.error);

  const dialogRef = useRef<HTMLDialogElement>(null);
  const [form, setForm] = useState(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  const [staged, setStaged] = useState<StagedCredential[]>([]);
  const [stagedLoaded, setStagedLoaded] = useState(false);
  const [credLoadFailed, setCredLoadFailed] = useState(false);
  const [snapshot, setSnapshot] = useState<string>("");
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  const [testingKey, setTestingKey] = useState<string | null>(null);
  const [testResults, setTestResults] = useState<Record<string, TestConnectionResult | undefined>>({});

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    let active = true;
    if (open) {
      setForm(editing ? { name: editing.name, host: editing.host, port: String(editing.port) } : { ...EMPTY_FORM });
      setFormError(null);
      setStaged([]);
      setStagedLoaded(!editing);
      setCredLoadFailed(false);
      setTestResults({});
      setConfirmDiscard(false);
      if (editing) {
        listEnvCredentials(editing.id)
          .then((creds) => {
            if (active) setStaged(fromStored(creds));
          })
          .catch(() => {
            if (active) setCredLoadFailed(true);
          })
          .finally(() => {
            if (active) setStagedLoaded(true);
          });
      }
      if (!dialog.open) dialog.showModal();
    } else {
      if (dialog.open) dialog.close();
      setStagedLoaded(false);
    }
    return () => {
      active = false;
    };
  }, [open, editing]);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const handleClose = () => onClose();
    dialog.addEventListener("close", handleClose);
    return () => dialog.removeEventListener("close", handleClose);
  }, [onClose]);

  useEffect(() => {
    if (open && stagedLoaded) setSnapshot(JSON.stringify(toInput(staged)));
  }, [open, stagedLoaded]);

  const dirty = stagedLoaded && snapshot !== "" && JSON.stringify(toInput(staged)) !== snapshot;

  const requestClose = () => {
    if (dirty) {
      setConfirmDiscard(true);
    } else {
      onClose();
    }
  };

  const handleSave = async () => {
    if (credLoadFailed) {
      setFormError("凭证加载失败，请关闭后重试");
      return;
    }
    if (!form.name.trim() || !form.host.trim()) {
      setFormError("名称 / 主机不能为空");
      return;
    }
    const port = parsePort(form.port);
    if (port === null) {
      setFormError("端口必须是 1-65535 的数字");
      return;
    }
    if (staged.length === 0) {
      setFormError("至少需要一条登录凭证");
      return;
    }
    if (staged.filter((c) => c.isDefault).length !== 1) {
      setFormError("必须恰好指定一个默认登录用户（点星标切换）");
      return;
    }
    const dup = staged.find((c, i) => staged.findIndex((o) => o.username.trim() === c.username.trim()) !== i);
    if (dup) {
      setFormError(`凭证用户名重复：${dup.username.trim()}`);
      return;
    }
    setSaving(true);
    setFormError(null);
    try {
      const ok = await save({
        environmentId: editing?.id ?? null,
        name: form.name.trim(),
        host: form.host.trim(),
        port,
        credentials: toInput(staged),
      });
      if (ok) onClose();
    } finally {
      setSaving(false);
    }
  };

  const handleTestCred = async (key: string) => {
    const c = staged.find((s) => s.key === key);
    if (!c) return;
    if (!form.host.trim()) {
      setFormError("主机不能为空");
      return;
    }
    const port = parsePort(form.port);
    if (port === null) {
      setFormError("端口必须是 1-65535 的数字");
      return;
    }
    setTestingKey(key);
    try {
      const result = await test({
        environmentId: c.id ? (editing?.id ?? null) : null,
        credentialId: c.id,
        host: form.host.trim(),
        port,
        user: c.username.trim(),
        authType: c.authType,
        privateKeyPath: c.authType === "private_key" ? c.privateKeyPath : null,
        password: c.secret && c.secret.trim() ? c.secret : null,
      });
      setTestResults((prev) => ({ ...prev, [key]: result ?? undefined }));
    } finally {
      setTestingKey(null);
    }
  };

  return (
    <>
      <dialog
        ref={dialogRef}
        aria-label={editing ? "编辑环境" : "新增环境"}
        className="z-50 w-[520px] max-w-[90vw] rounded-xl bg-card border border-border p-0 text-foreground overflow-hidden"
      >
        <div className="flex flex-col max-h-[85vh] overflow-hidden rounded-xl">
          <div className="flex items-center justify-between px-5 py-4 border-b border-border shrink-0">
            <h2 className="text-sm font-medium">{editing ? "编辑环境" : "新增环境"}</h2>
            <button
              onClick={requestClose}
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

            <div className="pt-2 border-t border-border space-y-2">
              <p className="text-xs text-muted-foreground">
                登录凭证：★ 为默认登录用户（日常连接使用）。目标 JVM 以其他用户运行时（arthas attach
                需要同用户），为该用户录入 SSH 凭证。
              </p>
              {credLoadFailed ? (
                <p role="alert" className="text-xs text-destructive py-2">
                  凭证加载失败，请关闭后重试
                </p>
              ) : stagedLoaded ? (
                <CredentialList
                  staged={staged}
                  testingId={testingKey}
                  testResults={testResults}
                  onSetDefault={(key) => setStaged((prev) => prev.map((c) => ({ ...c, isDefault: c.key === key })))}
                  onRemove={(key) => setStaged((prev) => prev.filter((c) => c.key !== key))}
                  onEditSave={(key, username, authType, privateKeyPath, secret) =>
                    setStaged((prev) =>
                      prev.map((c) =>
                        c.key === key
                          ? {
                              ...c,
                              username,
                              authType,
                              privateKeyPath: authType === "private_key" ? privateKeyPath : null,
                              secret: secret || c.secret,
                            }
                          : c,
                      ),
                    )
                  }
                  onTest={handleTestCred}
                  onAdd={(username, authType, privateKeyPath, secret, makeDefault) => {
                    if (!username) {
                      setFormError("凭证用户名不能为空");
                      return false;
                    }
                    if (authType === "private_key" && !privateKeyPath) {
                      setFormError("私钥认证需要填写私钥路径");
                      return false;
                    }
                    if (staged.some((c) => c.username.trim() === username)) {
                      setFormError(`凭证用户名已存在：${username}`);
                      return false;
                    }
                    setFormError(null);
                    setStaged((prev) => addStaged(prev, username, authType, privateKeyPath, secret, makeDefault));
                    return true;
                  }}
                />
              ) : (
                <div className="flex items-center justify-center gap-2 py-4 text-muted-foreground text-xs">
                  <CircleNotch size={14} className="animate-spin" aria-hidden="true" />
                  加载凭证…
                </div>
              )}
            </div>

            {(formError ?? storeError) && (
              <p role="alert" className="text-xs text-destructive break-words">
                {formError ?? storeError}
              </p>
            )}
          </div>

          <div className="flex items-center gap-2 px-5 py-4 border-t border-border shrink-0">
            <div className="flex-1" />
            <button
              onClick={requestClose}
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

      <DiscardChangesDialog
        open={confirmDiscard}
        onConfirm={() => {
          setConfirmDiscard(false);
          onClose();
        }}
        onCancel={() => setConfirmDiscard(false)}
      />
    </>
  );
}

const inputCls =
  "w-full bg-muted border border-border rounded-md text-sm text-foreground px-3 py-1.5 placeholder:text-muted-foreground/50 outline-none";

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
