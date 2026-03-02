FROM python:3.12-slim

WORKDIR /app

COPY requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt

COPY backend.py ./
COPY static/    static/

EXPOSE 8084

# Required at runtime:
#   JWT_SECRET  — must match SODP_JWT_SECRET on the SODP server
#   SODP_WS_URL — WebSocket URL the browser uses to reach the SODP server
#                 e.g. wss://sodp.example.com
#
# Optional:
#   PORT        — HTTP port (default: 8084)
CMD ["python", "backend.py"]
