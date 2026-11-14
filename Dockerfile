# syntax=docker/dockerfile:1
#
# MyDesk signaling server + app.
# No build step and no dependencies: node, server.js and MyDesk.html are all
# the image needs.

FROM node:22-alpine

ENV NODE_ENV=production \
    PORT=8080 \
    HOST=0.0.0.0

WORKDIR /app

COPY --chown=node:node package.json server.js MyDesk.html ./

USER node
EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD node -e "fetch('http://127.0.0.1:'+(process.env.PORT||8080)+'/healthz').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"

CMD ["node", "server.js"]
