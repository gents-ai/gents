/* The bridge's allowed-folder and plugin-approval commands return JSON it
   does not type (`serde_json::Value`); these are its shapes. */

export type AllowedFolderAccess = "read" | "read_write";

/** A host folder the agent's tools may use beyond the working folder. */
export type AllowedFolder = {
  path: string;
  access: AllowedFolderAccess;
};

export type AllowedFolders = {
  dirs: AllowedFolder[];
};

/** A running plugin call asking whether it may read a path outside the
    working folder and the allowed folders; the call waits for the answer. */
export type PluginApprovalRequest = {
  id: string;
  prompt: string;
  folder: string;
  isDir: boolean;
};

/** Allow this once, this file, the folder always, or deny. */
export type PluginApprovalDecision = "once" | "file" | "always" | "deny";
