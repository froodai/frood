// Shared queries: one place for each resource, so pages and the shell agree on the cache.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "./frood";
import type { Health, InboxEntry, Me, Run } from "./types";

export const useMe = () => useQuery({ queryKey: ["me"], queryFn: () => api<Me>("/v1/me"), staleTime: Infinity });

export const useHealth = () =>
  useQuery({ queryKey: ["health"], queryFn: () => api<Health>("/v1/health"), refetchInterval: 30_000, retry: false });

export const useInbox = () =>
  useQuery({ queryKey: ["inbox"], queryFn: () => api<InboxEntry[]>("/v1/inbox?limit=200"), refetchInterval: 30_000 });

export const useRuns = (filter: string) =>
  useQuery({
    queryKey: ["runs", filter],
    queryFn: () => api<Run[]>(`/v1/runs?limit=100${filter ? `&status=${filter}` : ""}`),
    refetchInterval: 15_000,
  });

/** Starts a job by name, e.g. `backup`. */
export function useStartJob() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (job: string) => api<Run>("/v1/runs", { method: "POST", json: { job } }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["runs"] }),
  });
}
