{{- /*
Shared pod template body, used by both statefulset.yaml (db.mode=sqlite)
and deployment.yaml (db.mode=postgres) -- the container args/env/volumes
logic is identical either way except for how the durability logs and the
data directory are backed, which is exactly what differs between those
two resource kinds already. Call with the root context (`.`).
*/ -}}
{{- define "nirdosha.podTemplate" -}}
metadata:
  labels:
    {{- include "nirdosha.selectorLabels" . | nindent 4 }}
spec:
  serviceAccountName: {{ include "nirdosha.serviceAccountName" . }}
  securityContext:
    {{- toYaml .Values.podSecurityContext | nindent 4 }}
  terminationGracePeriodSeconds: {{ .Values.terminationGracePeriodSeconds }}
  containers:
    - name: nirdosha
      image: "{{ .Values.image.repository }}:{{ .Values.image.tag }}"
      imagePullPolicy: {{ .Values.image.pullPolicy }}
      args:
        - serve
        - /data/{{ .Values.entrypointFile }}
        - --host
        - 0.0.0.0
        - --port
        - "8080"
        {{- if eq .Values.db.mode "postgres" }}
        - --transact-log
        - $(TRANSACT_LOG_URL)
        - --workflow-log
        - $(WORKFLOW_LOG_URL)
        {{- else }}
        - --transact-log
        - /data/{{ include "nirdosha.name" . }}.transact.db
        - --workflow-log
        - /data/{{ include "nirdosha.name" . }}.workflow.db
        {{- end }}
        {{- if .Values.auth.enabled }}
        - --jwks-file
        - /etc/nirdosha/jwks/jwks.json
        - --issuer
        - {{ .Values.auth.issuer | quote }}
        - --audience
        - {{ .Values.auth.audience | quote }}
        {{- end }}
        {{- if .Values.presence.enabled }}
        - --presence-token-file
        - /etc/nirdosha/presence/token
        {{- end }}
        {{- if .Values.otel.enabled }}
        - --otel-port
        - {{ .Values.otel.port | quote }}
        - --otel-token-file
        - /etc/nirdosha/otel/token
        {{- end }}
        - --theme
        - /data/theme.json
      ports:
        - name: http
          containerPort: 8080
        {{- if .Values.otel.enabled }}
        - name: otel
          containerPort: {{ .Values.otel.port }}
        {{- end }}
      {{- if eq .Values.db.mode "postgres" }}
      env:
        - name: TRANSACT_LOG_URL
          valueFrom:
            secretKeyRef:
              name: {{ .Values.db.postgresSecretName }}
              key: transact-log-url
        - name: WORKFLOW_LOG_URL
          valueFrom:
            secretKeyRef:
              name: {{ .Values.db.postgresSecretName }}
              key: workflow-log-url
      {{- end }}
      securityContext:
        {{- toYaml .Values.securityContext | nindent 8 }}
      resources:
        {{- toYaml .Values.resources | nindent 8 }}
      livenessProbe:
        httpGet:
          path: /healthz
          port: http
        initialDelaySeconds: 2
        periodSeconds: 10
        failureThreshold: 3
      readinessProbe:
        # Doubles as the startup probe (KUBERNETES.md: "same route as
        # readiness, with a longer failureThreshold, works as a startup
        # probe") -- a generous budget for schema migration + crash
        # replay to finish before the Pod is ever marked NotReady long
        # enough to be killed.
        httpGet:
          path: /readyz
          port: http
        initialDelaySeconds: 1
        periodSeconds: 5
        failureThreshold: 30
      volumeMounts:
        - name: data
          mountPath: /data
        {{- if .Values.auth.enabled }}
        - name: jwks
          mountPath: /etc/nirdosha/jwks
          readOnly: true
        {{- end }}
        {{- if .Values.presence.enabled }}
        - name: presence-token
          mountPath: /etc/nirdosha/presence
          readOnly: true
        {{- end }}
        {{- if .Values.otel.enabled }}
        - name: otel-token
          mountPath: /etc/nirdosha/otel
          readOnly: true
        {{- end }}
  volumes:
    {{- if eq .Values.db.mode "postgres" }}
    - name: data
      emptyDir: {}
    {{- end }}
    {{- if .Values.auth.enabled }}
    - name: jwks
      secret:
        secretName: {{ .Values.auth.jwksSecretName }}
        items:
          - key: jwks.json
            path: jwks.json
    {{- end }}
    {{- if .Values.presence.enabled }}
    - name: presence-token
      secret:
        secretName: {{ .Values.presence.tokenSecretName }}
    {{- end }}
    {{- if .Values.otel.enabled }}
    - name: otel-token
      secret:
        secretName: {{ .Values.otel.tokenSecretName }}
    {{- end }}
  {{- with .Values.nodeSelector }}
  nodeSelector:
    {{- toYaml . | nindent 4 }}
  {{- end }}
  {{- with .Values.affinity }}
  affinity:
    {{- toYaml . | nindent 4 }}
  {{- end }}
  {{- with .Values.tolerations }}
  tolerations:
    {{- toYaml . | nindent 4 }}
  {{- end }}
{{- end -}}
