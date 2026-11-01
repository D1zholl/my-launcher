import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorMessage } from "../lib/common";
import { MOD_LOADER_TYPES } from "../types";
import type { LoaderVersion, ModLoaderType } from "../types";

const LOADER_CACHE_TTL_MS = 5 * 60 * 1000;
const loaderVersionsCache = new Map<string, { expiresAt: number; request: Promise<LoaderVersion[]> }>();

function getLoaderVersions(loaderType: string, minecraftVersion: string): Promise<LoaderVersion[]> {
  const now = Date.now();
  for (const [cachedKey, entry] of loaderVersionsCache) {
    if (entry.expiresAt <= now) loaderVersionsCache.delete(cachedKey);
  }
  const key = `${loaderType}|${minecraftVersion}`;
  const cached = loaderVersionsCache.get(key);
  if (cached) return cached.request;

  const request = invoke<LoaderVersion[]>("get_loader_versions", { loaderType, minecraftVersion });
  loaderVersionsCache.set(key, { expiresAt: Date.now() + LOADER_CACHE_TTL_MS, request });
  void request.catch(() => {
    if (loaderVersionsCache.get(key)?.request === request) loaderVersionsCache.delete(key);
  });
  return request;
}

export function useLoaderOptions(version: string, loader: string) {
  const [availableLoaderTypes, setAvailableLoaderTypes] = useState<ModLoaderType[]>([]);
  const [availabilityFor, setAvailabilityFor] = useState<string | null>(null);
  const [loaderAvailabilityLoading, setLoaderAvailabilityLoading] = useState(false);
  const [loaderVersions, setLoaderVersions] = useState<LoaderVersion[]>([]);
  const [loaderVersionsFor, setLoaderVersionsFor] = useState<string | null>(null);
  const [loaderLoading, setLoaderLoading] = useState(false);
  const [loaderError, setLoaderError] = useState("");

  useEffect(() => {
    if (!version) {
      setAvailableLoaderTypes([]);
      setAvailabilityFor(null);
      setLoaderAvailabilityLoading(false);
      return;
    }

    let active = true;
    setAvailableLoaderTypes([]);
    setAvailabilityFor(null);
    setLoaderAvailabilityLoading(true);
    Promise.all(
      MOD_LOADER_TYPES.map(async (loaderType) => {
        try {
          const items = await getLoaderVersions(loaderType, version);
          return items.length > 0 ? loaderType : null;
        } catch {
          return null;
        }
      }),
    )
      .then((results) => {
        if (!active) return;
        setAvailableLoaderTypes(
          results.filter((loaderType): loaderType is ModLoaderType => loaderType !== null),
        );
        setAvailabilityFor(version);
      })
      .finally(() => {
        if (active) setLoaderAvailabilityLoading(false);
      });

    return () => {
      active = false;
    };
  }, [version]);

  useEffect(() => {
    if (loader === "vanilla" || !version) {
      setLoaderVersions([]);
      setLoaderVersionsFor(null);
      setLoaderLoading(false);
      setLoaderError("");
      return;
    }
    let active = true;
    const key = `${version}|${loader}`;
    setLoaderVersionsFor(null);
    setLoaderLoading(true);
    setLoaderError("");
    getLoaderVersions(loader, version)
      .then((items) => {
        if (!active) return;
        setLoaderVersions(items);
        setLoaderVersionsFor(key);
        if (items.length === 0) setLoaderError("Для этой версии Minecraft загрузчик не найден.");
      })
      .catch((error: unknown) => {
        if (active) setLoaderError(errorMessage(error));
      })
      .finally(() => {
        if (active) setLoaderLoading(false);
      });
    return () => {
      active = false;
    };
  }, [loader, version]);

  return {
    availableLoaderTypes,
    availabilityFor,
    loaderAvailabilityLoading,
    loaderVersions,
    loaderVersionsFor,
    loaderLoading,
    loaderError,
  };
}
