import type { createDesktopInvoker } from "./invoke.js";
import type { DesktopApiAdapter, JoinRequestCommand } from "./types.js";

export function createJoinRequestCommands(
  invokeDesktop: ReturnType<typeof createDesktopInvoker>,
): Pick<DesktopApiAdapter, JoinRequestCommand> {
  return {
    listJoinRequests: (nodeDid) =>
      invokeDesktop("desktop_join_requests_list", { nodeDid }),
    decideJoinRequest: (request) =>
      invokeDesktop("desktop_join_request_decide", { request }),
  };
}
