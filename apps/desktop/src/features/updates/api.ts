import { invokeCommand } from "../../shared/api/tauri";
import type { AppUpdateInfo } from "../../types";

export function checkAppUpdate() {
  return invokeCommand<AppUpdateInfo>("check_app_update");
}
