import { invoke } from "@tauri-apps/api/core";
import { getDatabase, saveSetting, setActiveAccount } from "./database";
import type { Account, DeviceCodeInfo } from "../types";

const ACCOUNT_COLUMNS = "INSERT INTO accounts (id, username, uuid, account_type) VALUES ($1, $2, $3, $4)";

export async function createOfflineAccount(username: string): Promise<Account> {
  // Java derives offline UUIDs from "OfflinePlayer:<name>", so the backend
  // computes the same stable UUID instead of a random one.
  const uuid = await invoke<string>("offline_account_uuid", { username });
  const account: Account = {
    id: crypto.randomUUID(),
    username,
    uuid,
    account_type: "offline",
  };
  const database = await getDatabase();
  await database.execute(ACCOUNT_COLUMNS, [account.id, account.username, account.uuid, account.account_type]);
  await setActiveAccount(account.id);
  return account;
}

export async function requestMicrosoftDeviceCode(clientId: string): Promise<DeviceCodeInfo> {
  return invoke<DeviceCodeInfo>("request_microsoft_device_code", { clientId });
}

export async function completeMicrosoftAccount(flowId: string): Promise<Account> {
  const account = await invoke<Account>("complete_microsoft_login", { flowId });
  try {
    const database = await getDatabase();
    // Re-authenticating an existing Microsoft account must update it instead of
    // failing on the primary key.
    await database.execute(
      `${ACCOUNT_COLUMNS} ON CONFLICT(id) DO UPDATE SET username = excluded.username, uuid = excluded.uuid, account_type = excluded.account_type`,
      [account.id, account.username, account.uuid, account.account_type],
    );
    await setActiveAccount(account.id);
    return account;
  } catch (error) {
    // Do not leave freshly issued tokens in the credential store when the
    // account could not be persisted.
    await invoke("remove_microsoft_tokens", { accountId: account.id }).catch(() => undefined);
    throw error;
  }
}

export async function saveMicrosoftClientId(clientId: string): Promise<void> {
  await saveSetting("microsoft_client_id", clientId);
}

export async function selectAccount(accountId: string): Promise<void> {
  await setActiveAccount(accountId);
}

export async function deleteAccount(account: Account): Promise<void> {
  if (account.account_type === "microsoft") {
    await invoke("remove_microsoft_tokens", { accountId: account.id });
  }
  const database = await getDatabase();
  await database.execute("DELETE FROM accounts WHERE id = $1", [account.id]);
}
