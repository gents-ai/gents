/* The deployment the transcript is being read under. Its rows are too deep
   to be handed it, and they need it to name a behavior: a worker's mark is
   its agent's, and an agent is named by the deployment it belongs to. */
import { createContext, useContext } from "react";
import type { DeploymentView } from "@source-inc/gents-desktop-client";

export const DeploymentContext = createContext<DeploymentView | null>(null);
export const useDeployment = () => useContext(DeploymentContext);
