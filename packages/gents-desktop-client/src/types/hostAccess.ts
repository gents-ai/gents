export type AllowedFolderAccess = "read" | "read_write";

export type AllowedFolder = {
  path: string;
  access: AllowedFolderAccess;
};

export type AllowedFolders = {
  dirs: AllowedFolder[];
};

export type PluginApprovalRequest = {
  id: string;
  prompt: string;
  folder: string;
  isDir: boolean;
};

export type PluginApprovalDecision = "once" | "file" | "always" | "deny";
