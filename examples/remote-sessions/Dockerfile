FROM node:22.23.2-bookworm-slim
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=dev --omit=optional --ignore-scripts
COPY *.ts ./
RUN install -d -o node -g node /app/data
ENV REMOTE_HOST=0.0.0.0 REMOTE_PORT=4174 REMOTE_STATE_PATH=/app/data/control.sqlite
USER node
EXPOSE 4174
VOLUME ["/app/data"]
HEALTHCHECK --interval=30s --timeout=5s CMD ["node", "-e", "fetch('http://127.0.0.1:4174/healthz').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"]
CMD ["npm", "start"]
