export type FlueServerState = "starting" | "ready" | "failed" | "stopped";

export type FlueServerStatus = {
  state: FlueServerState;
  baseUrl: string | null;
  message: string | null;
  pid: number | null;
};

export type FlueConnection = {
  baseUrl: string;
  token: string;
  status: FlueServerStatus;
};
