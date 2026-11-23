{{/* The release's name for its objects, at most 63 characters with room for suffixes. */}}
{{- define "frood.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 40 | trimSuffix "-" -}}
{{- else if contains "frood" .Release.Name -}}
{{- .Release.Name | trunc 40 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-frood" .Release.Name | trunc 40 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{/* Labels of every object; a component adds app.kubernetes.io/component. */}}
{{- define "frood.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
app.kubernetes.io/name: frood
app.kubernetes.io/part-of: frood
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/* Selector labels of one component: (dict "ctx" $ "component" "server"). */}}
{{- define "frood.selector" -}}
app.kubernetes.io/name: frood
app.kubernetes.io/instance: {{ .ctx.Release.Name }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{/* An image: (dict "ctx" $ "image" .Values.x.image). The tag defaults to appVersion. */}}
{{- define "frood.image" -}}
{{- printf "%s:%s" .image.repository (default .ctx.Chart.AppVersion .image.tag) -}}
{{- end -}}

{{- define "frood.publicUrl" -}}
{{- if .Values.server.publicUrl -}}
{{- .Values.server.publicUrl | trimSuffix "/" -}}
{{- else if .Values.ingress.enabled -}}
{{- printf "https://%s" (required "ingress.host is required" .Values.ingress.host) -}}
{{- end -}}
{{- end -}}

{{/* Where Frood is reached inside the cluster, from any namespace. */}}
{{- define "frood.internalUrl" -}}
{{- printf "http://%s.%s.svc:%v" (include "frood.fullname" .) .Release.Namespace .Values.service.port -}}
{{- end -}}

{{- define "frood.moduleTokensSecret" -}}
{{- default (printf "%s-module-tokens" (include "frood.fullname" .)) .Values.moduleTokens.existingSecret -}}
{{- end -}}

{{/* The Secret with the values of ${secret:NAME}, if any. */}}
{{- define "frood.secretsSecret" -}}
{{- if .Values.secrets.existingSecret -}}
{{- .Values.secrets.existingSecret -}}
{{- else if .Values.secrets.create -}}
{{- printf "%s-secrets" (include "frood.fullname" .) -}}
{{- end -}}
{{- end -}}

{{- define "frood.tokenKey" -}}
{{- printf "FROOD_MODULE_TOKEN_%s" (. | upper | replace "-" "_") -}}
{{- end -}}

{{/* The modules this release runs, as a list of their names in frood.yaml. */}}
{{- define "frood.modules" -}}
{{- $names := list -}}
{{- if .Values.agents.enabled }}{{ $names = append $names .Values.agents.name }}{{ end -}}
{{- if .Values.telegram.enabled }}{{ $names = append $names .Values.telegram.name }}{{ end -}}
{{- if .Values.backup.enabled }}{{ $names = append $names .Values.backup.name }}{{ end -}}
{{- toJson $names -}}
{{- end -}}

{{/* runtime-k8s's settings (FROOD_MODULE_CONFIG). */}}
{{- define "frood.agentsConfig" -}}
{{- $agents := dict -}}
{{- range $agent, $repository := .Values.agents.agentImages -}}
{{- if $repository -}}
{{- $image := ternary $repository (printf "%s:%s" $repository $.Chart.AppVersion) (contains ":" (base $repository)) -}}
{{- $_ := set $agents $agent (dict "image" $image) -}}
{{- end -}}
{{- end -}}
{{- $pull := list -}}
{{- range .Values.imagePullSecrets }}{{ $pull = append $pull .name }}{{ end -}}
{{- $config := deepCopy (default dict .Values.agents.config) -}}
{{- /* The release's labels win: the NetworkPolicy for agent pods selects them. */ -}}
{{- $labels := merge (dict "app.kubernetes.io/part-of" "frood" "app.kubernetes.io/instance" .Release.Name) (default dict $config.labels) -}}
{{- $_ := set $config "agents" $agents -}}
{{- $_ := set $config "frood_url" (include "frood.internalUrl" .) -}}
{{- $_ := set $config "labels" $labels -}}
{{- if .Values.agents.jobNamespace }}{{ $_ := set $config "namespace" .Values.agents.jobNamespace }}{{ end -}}
{{- if $pull }}{{ $_ := set $config "image_pull_secrets" $pull }}{{ end -}}
{{- toJson $config -}}
{{- end -}}

{{/* frood.yaml: `config` with what the cluster decides. */}}
{{- define "frood.config" -}}
{{- $c := deepCopy .Values.config -}}
{{- $server := default dict $c.server -}}
{{- $_ := set $server "listen" (printf "0.0.0.0:%v" .Values.server.port) -}}
{{- with include "frood.publicUrl" . }}{{ $_ := set $server "public_url" . }}{{ end -}}
{{- $_ := set $c "server" $server -}}
{{- $storage := default dict $c.storage -}}
{{- $_ := set $storage "uses" "postgres" -}}
{{- $_ := set $storage "url" "${secret:FROOD_DATABASE_URL}" -}}
{{- if eq .Values.files.type "pvc" -}}
{{- $_ := set $storage "objects" (dict "uses" "disk" "max_mb" .Values.files.max_mb) -}}
{{- else if eq .Values.files.type "database" -}}
{{- $_ := set $storage "objects" (dict "uses" "database" "max_mb" (min .Values.files.max_mb 512)) -}}
{{- else if eq .Values.files.type "s3" -}}
{{- if not (and $storage.objects (eq (toString $storage.objects.uses) "s3")) -}}
{{- fail "files.type s3: set config.storage.objects to { uses: s3, endpoint, bucket, access_key_id, secret_access_key }" -}}
{{- end -}}
{{- else -}}
{{- fail "files.type is pvc, s3 or database" -}}
{{- end -}}
{{- $_ := set $c "storage" $storage -}}
{{- $_ := set $c "deploy" (dict "server" "container" "image" (include "frood.image" (dict "ctx" . "image" .Values.image))) -}}
{{- $modules := default dict $c.modules -}}
{{- $fullname := include "frood.fullname" . -}}
{{- if .Values.agents.enabled -}}
{{- $_ := set $modules .Values.agents.name (dict "url" (printf "http://%s-%s:8080" $fullname .Values.agents.name) "config" (include "frood.agentsConfig" . | fromJson)) -}}
{{- end -}}
{{- if .Values.telegram.enabled -}}
{{- $_ := set $modules .Values.telegram.name (dict "url" (printf "http://%s-%s:8080" $fullname .Values.telegram.name) "config" (default dict .Values.telegram.config)) -}}
{{- end -}}
{{- if .Values.backup.enabled -}}
{{- $_ := set $modules .Values.backup.name (dict "url" (printf "http://%s-%s:8080" $fullname .Values.backup.name)) -}}
{{- end -}}
{{- if $modules }}{{ $_ := set $c "modules" $modules }}{{ end -}}
{{- toYaml $c -}}
{{- end -}}

{{/* The security context of every container. */}}
{{- define "frood.containerSecurity" -}}
{{- toYaml .Values.securityContext -}}
{{- end -}}

{{/* Env of a module: (dict "ctx" $ "name" "agents" "config" "{...json...}"). */}}
{{- define "frood.moduleEnv" -}}
- name: FROOD_URL
  value: {{ printf "http://%s:%v" (include "frood.fullname" .ctx) .ctx.Values.service.port | quote }}
- name: FROOD_MODULE
  value: {{ .name | quote }}
- name: FROOD_TOKEN
  valueFrom:
    secretKeyRef:
      name: {{ include "frood.moduleTokensSecret" .ctx }}
      key: {{ include "frood.tokenKey" .name }}
- name: FROOD_MODULE_CONFIG
  value: {{ .config | quote }}
- name: FROOD_DATA
  value: /data
- name: PORT
  value: "8080"
{{- end -}}

{{/* A module's probes. */}}
{{- define "frood.moduleProbes" -}}
readinessProbe:
  httpGet: { path: /healthz, port: http }
  periodSeconds: 10
livenessProbe:
  httpGet: { path: /healthz, port: http }
  periodSeconds: 30
  failureThreshold: 5
{{- end -}}

{{/* NetworkPolicy egress rules: the DNS, the internet, a list of { cidr, port }. */}}
{{- define "frood.np.dns" -}}
- to:
    {{- toYaml . | nindent 4 }}
  ports:
    - { protocol: UDP, port: 53 }
    - { protocol: TCP, port: 53 }
{{- end -}}
{{- define "frood.np.internet" -}}
- to:
    - ipBlock:
        cidr: {{ .cidr }}
        {{- with .except }}
        except:
          {{- toYaml . | nindent 10 }}
        {{- end }}
{{- end -}}
{{- define "frood.np.blocks" -}}
{{- range . }}
- to:
    - ipBlock: { cidr: {{ .cidr }} }
  {{- if .port }}
  ports:
    - { protocol: TCP, port: {{ .port }} }
  {{- end }}
{{- end }}
{{- end -}}
