/* The app's screens, one per route. Importing this module once (App does)
   fills the route registry. */
import { registerRouteScreens } from "@/app/routeScreens";
import { AgentScreen } from "./agent/AgentScreen";
import { AgentsScreen } from "./AgentsScreen";
import { MailboxScreen } from "./MailboxScreen";
import { SessionScreen } from "./SessionScreen";
import { SessionsScreen } from "./SessionsScreen";

registerRouteScreens({
  sessions: (route) => <SessionsScreen nodeDid={route.nodeDid} />,
  session: () => <SessionScreen />,
  mailbox: (route) => <MailboxScreen nodeDid={route.nodeDid} />,
  agents: () => <AgentsScreen />,
  nodes: () => <AgentsScreen />,
  agent: (route) => (
    <AgentScreen agentDid={route.agentDid} section={route.section} item={route.item} />
  ),
});
