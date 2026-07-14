# ---- build stage ----------------------------------------------------------
FROM node:22-alpine AS build
WORKDIR /app

# Install deps first for better layer caching.
COPY package.json package-lock.json* ./
RUN npm ci || npm install

# Build.
COPY . .
RUN npm run build

# ---- serve stage ----------------------------------------------------------
FROM nginx:1.27-alpine AS serve

# gettext provides `envsubst` used by the entrypoint.
RUN apk add --no-cache gettext

# Nginx site config.
COPY nginx.conf /etc/nginx/conf.d/default.conf

# Built SPA.
COPY --from=build /app/dist /usr/share/nginx/html

# Runtime config template (config.js is generated from it at container start).
COPY static/config.template.js /usr/share/nginx/html/config.template.js

# Entrypoint injects config.js then execs nginx.
COPY docker-entrypoint.sh /docker-entrypoint.d/99-sauron-config.sh
RUN chmod +x /docker-entrypoint.d/99-sauron-config.sh

# The official nginx image already runs /docker-entrypoint.d/*.sh before starting.
# We also expose a standalone entrypoint for non-nginx-base use.
COPY docker-entrypoint.sh /usr/local/bin/sauron-entrypoint.sh
RUN chmod +x /usr/local/bin/sauron-entrypoint.sh

ENV API_BASE_URL=http://localhost:8080
ENV INGEST_BASE_URL=http://localhost:8081

EXPOSE 80
ENTRYPOINT ["/usr/local/bin/sauron-entrypoint.sh"]
CMD ["nginx", "-g", "daemon off;"]
