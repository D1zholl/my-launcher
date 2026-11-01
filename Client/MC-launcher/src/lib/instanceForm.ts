import type { InstanceForm } from "../types";

export const EMPTY_INSTANCE_FORM: InstanceForm = {
  name: "",
  avatarData: null,
  version: "",
  loader: "vanilla",
  loaderVersion: "",
  memoryMb: 4096,
  jvmArgs: "",
  windowWidth: 1280,
  windowHeight: 720,
  fullscreen: false,
};

export function clampMemory(memoryMb: number, limitMb: number): number {
  return Math.max(512, Math.min(memoryMb, limitMb));
}
