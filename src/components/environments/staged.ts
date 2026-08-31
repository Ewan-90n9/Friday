import type { CredentialInput, EnvCredentialRow, EnvironmentAuthType } from "@/lib/types";

/** 弹窗内暂存的凭证（key = 前端行 key；已存凭证 = id，新凭证 = "new-N"） */
export interface StagedCredential {
  key: string;
  /** 已存凭证的 id；新凭证为 null */
  id: string | null;
  username: string;
  authType: EnvironmentAuthType;
  privateKeyPath: string | null;
  /** 新填的 secret；null/"" = 不修改 */
  secret: string | null;
  isDefault: boolean;
}

let seq = 0;
function nextKey(): string {
  seq += 1;
  return `new-${seq}`;
}

export function fromStored(creds: EnvCredentialRow[]): StagedCredential[] {
  return creds.map((c) => ({
    key: c.id,
    id: c.id,
    username: c.username,
    authType: c.auth_type,
    privateKeyPath: c.private_key_path,
    secret: null,
    isDefault: c.is_default,
  }));
}

export function toInput(staged: StagedCredential[]): CredentialInput[] {
  return staged.map((c) => ({
    id: c.id,
    username: c.username,
    authType: c.authType,
    privateKeyPath: c.authType === "private_key" ? c.privateKeyPath : null,
    secret: c.secret && c.secret.trim() ? c.secret : null,
    isDefault: c.isDefault,
  }));
}

export function addStaged(
  staged: StagedCredential[],
  username: string,
  authType: EnvironmentAuthType,
  privateKeyPath: string,
  secret: string,
  makeDefault: boolean,
): StagedCredential[] {
  const entry: StagedCredential = {
    key: nextKey(),
    id: null,
    username,
    authType,
    privateKeyPath: authType === "private_key" ? privateKeyPath : null,
    secret: secret || null,
    isDefault: makeDefault,
  };
  const next = [...staged, entry];
  return makeDefault ? next.map((c) => ({ ...c, isDefault: c.key === entry.key })) : next;
}
