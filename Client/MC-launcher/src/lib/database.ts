import { invoke } from "@tauri-apps/api/core";
import { DEFAULT_UI_PALETTES, parseUiPalettes } from "./palettes";
import type {
  Account,
  DatabaseState,
  Instance,
  Theme,
  ThemePalettes,
  ThemeRow,
} from "../types";

let databaseInitialization: Promise<DatabaseState> | undefined;
interface DatabaseExecuteResult {
  rowsAffected: number;
  lastInsertId: number;
}

interface DatabaseFacade {
  execute(query: string, bindValues?: unknown[]): Promise<DatabaseExecuteResult>;
  select<T>(query: string, bindValues?: unknown[]): Promise<T>;
}

const database: DatabaseFacade = {
  execute: (query, bindValues = []) => invoke("database_execute", { query, bindValues }),
  select: <T>(query: string, bindValues: unknown[] = []) =>
    invoke<T>("database_select", { query, bindValues }),
};

export function getDatabase(): Promise<DatabaseFacade> {
  return Promise.resolve(database);
}

export async function saveSetting(key: string, value: string): Promise<void> {
  const database = await getDatabase();
  await database.execute(
    "INSERT INTO settings (key, value) VALUES ($1, $2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    [key, value],
  );
}

export async function setActiveAccount(accountId: string): Promise<void> {
  await saveSetting("active_account_id", accountId);
}

export async function clearActiveAccount(): Promise<void> {
  const database = await getDatabase();
  await database.execute("DELETE FROM settings WHERE key = 'active_account_id'");
}

export function initializeDatabase(): Promise<DatabaseState> {
  if (!databaseInitialization) {
    databaseInitialization = (async () => {
    await invoke("migrate_database_schema");
    const database = await getDatabase();
    const schemaVersionRows = await database.select<{ user_version: number }[]>("PRAGMA user_version");
    const schemaVersion = schemaVersionRows[0]?.user_version ?? 0;
    if (schemaVersion < 2) {
      const legacyAvatars = await database.select<{ id: string; avatar_data: string }[]>(
        "SELECT id, avatar_data FROM instances WHERE avatar_data LIKE 'data:image/%;base64,%'",
      );
      let migrationComplete = true;
      for (const avatar of legacyAvatars) {
        try {
          const path = await invoke<string>("save_instance_avatar", {
            instanceId: avatar.id,
            dataUrl: avatar.avatar_data,
          });
          await database.execute("UPDATE instances SET avatar_data = $1 WHERE id = $2", [path, avatar.id]);
        } catch {
          migrationComplete = false;
        }
      }
      if (migrationComplete) await database.execute("PRAGMA user_version = 2");
    }
    await database.execute(
      "INSERT INTO settings (key, value) VALUES ('theme', 'light') ON CONFLICT(key) DO NOTHING",
    );
    const legacyPaletteRows = await database.select<ThemeRow[]>(
      "SELECT value FROM settings WHERE key = 'accent_palette'",
    );
    const legacyAccentColors: Record<string, string> = {
      forest: "#557b52",
      ocean: "#397f91",
      iris: "#8264a8",
      amber: "#b87932",
      berry: "#b45468",
    };
    const legacyAccent = legacyAccentColors[legacyPaletteRows[0]?.value ?? ""];
    const initialUiPalettes: ThemePalettes = legacyAccent
      ? {
          light: { ...DEFAULT_UI_PALETTES.light, accent: legacyAccent, button: legacyAccent },
          dark: { ...DEFAULT_UI_PALETTES.dark, accent: legacyAccent, button: legacyAccent },
          custom: { ...DEFAULT_UI_PALETTES.custom, accent: legacyAccent, button: legacyAccent },
        }
      : DEFAULT_UI_PALETTES;
    await database.execute(
      "INSERT INTO settings (key, value) VALUES ('ui_palette', $1) ON CONFLICT(key) DO NOTHING",
      [JSON.stringify(initialUiPalettes)],
    );
    await database.execute(
      "INSERT INTO settings (key, value) VALUES ('microsoft_client_id', '') ON CONFLICT(key) DO NOTHING",
    );

    const [instances, accounts, activeRows, themeRows, paletteRows, clientIdRows] = await Promise.all([
      database.select<Instance[]>("SELECT * FROM instances ORDER BY created_at DESC"),
      database.select<Account[]>("SELECT * FROM accounts ORDER BY created_at DESC"),
      database.select<ThemeRow[]>("SELECT value FROM settings WHERE key = 'active_account_id'"),
      database.select<ThemeRow[]>("SELECT value FROM settings WHERE key = 'theme'"),
      database.select<ThemeRow[]>("SELECT value FROM settings WHERE key = 'ui_palette'"),
      database.select<ThemeRow[]>("SELECT value FROM settings WHERE key = 'microsoft_client_id'"),
    ]);
    const uiPalettes = parseUiPalettes(paletteRows[0]?.value);
    const savedTheme = themeRows[0]?.value;
    const selectedTheme: Theme = savedTheme === "dark" || savedTheme === "custom" ? savedTheme : "light";
    const hasManualPalette = selectedTheme !== "custom"
      && JSON.stringify(uiPalettes[selectedTheme]) !== JSON.stringify(DEFAULT_UI_PALETTES[selectedTheme]);
    const storedPalettes: ThemePalettes = hasManualPalette
      ? { ...uiPalettes, custom: uiPalettes[selectedTheme] }
      : uiPalettes;
    return {
      accounts,
      activeAccountId: activeRows[0]?.value ?? "",
      microsoftClientId: clientIdRows[0]?.value ?? "",
      instances,
      theme: hasManualPalette ? "custom" : selectedTheme,
      uiPalettes: storedPalettes,
    };
    })().catch((error: unknown) => {
      databaseInitialization = undefined;
      throw error;
    });
  }

  return databaseInitialization;
}
