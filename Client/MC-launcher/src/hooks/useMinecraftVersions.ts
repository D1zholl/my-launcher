import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorMessage } from "../lib/common";
import type { MinecraftVersions } from "../types";

export function useMinecraftVersions() {
  const [versions, setVersions] = useState<string[]>([]);
  const [latest, setLatest] = useState("");
  const [versionError, setVersionError] = useState("");

  useEffect(() => {
    let active = true;
    // The manifest is fetched by Rust so the webview does not need a broad
    // `connect-src` exception in the content security policy.
    invoke<MinecraftVersions>("get_minecraft_versions")
      .then((manifest) => {
        if (!active) return;
        setVersions(manifest.versions);
        setLatest(manifest.latest);
      })
      .catch((error: unknown) => {
        if (active) setVersionError(errorMessage(error, "Не удалось загрузить список версий Minecraft."));
      });
    return () => {
      active = false;
    };
  }, []);

  return { versions, latest, versionError };
}
