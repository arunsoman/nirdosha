{{- define "nirdosha.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "nirdosha.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name (include "nirdosha.name" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "nirdosha.labels" -}}
app.kubernetes.io/name: {{ include "nirdosha.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{- end -}}

{{- define "nirdosha.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nirdosha.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "nirdosha.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{ default (include "nirdosha.fullname" .) .Values.serviceAccount.name }}
{{- else -}}
{{ default "default" .Values.serviceAccount.name }}
{{- end -}}
{{- end -}}

{{- /*
The one guard this chart enforces at render time (mirrors
plugins/deploy_targets/kubernetes.py::render_manifests's own ValueError):
replicaCount > 1 with db.mode=sqlite is a correctness bug, not a scaling
config -- N replicas would each hold their own divergent SQLite
durability log.
*/ -}}
{{- define "nirdosha.validateReplicaMode" -}}
{{- if and (gt (.Values.replicaCount | int) 1) (ne .Values.db.mode "postgres") -}}
{{- fail "replicaCount > 1 requires db.mode=postgres (see KUBERNETES.md's P1 remediation item) -- one replica per independent SQLite durability log is a correctness bug, not a scaling config" -}}
{{- end -}}
{{- if and (eq .Values.db.mode "postgres") (not .Values.db.postgresSecretName) -}}
{{- fail "db.mode=postgres requires db.postgresSecretName (a Secret with transact-log-url/workflow-log-url keys)" -}}
{{- end -}}
{{- end -}}
