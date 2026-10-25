// Spheres of life (docs/26 §3): a label on knowledge for reading and filters. The built-in ones
// are named in the interface's language; an installation's own come with their names.

import { useQuery } from "@tanstack/react-query";
import { BriefcaseBusiness, CodeXml, House, type LucideIcon, Tag } from "lucide-react";
import { useI18n } from "@/i18n";
import type { Key } from "@/i18n/en";
import { api } from "./frood";

export type Sphere = { id: string; name: string | null; about: string | null; built_in: boolean };

const BUILT_IN: Record<string, LucideIcon> = { tech: CodeXml, business: BriefcaseBusiness, personal: House };

export const sphereIcon = (id: string): LucideIcon => BUILT_IN[id] ?? Tag;

export const useSpheres = () => useQuery({ queryKey: ["spheres"], queryFn: () => api<Sphere[]>("/v1/spheres"), staleTime: Infinity });

/** A sphere's name: the interface's for built-in ones, the installation's for its own. */
export function useSphereName() {
  const { t } = useI18n();
  const spheres = useSpheres().data;
  return (id: string) => {
    const sphere = spheres?.find((s) => s.id === id);
    if (sphere?.name) return sphere.name;
    return id in BUILT_IN ? t(`sphere.${id}` as Key) : id;
  };
}
