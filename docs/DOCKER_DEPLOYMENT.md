# Docker Deployment Guide

This guide explains how to deploy `perps-stats` using Docker and Docker Compose for both development and production environments.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Quick Start](#quick-start)
- [Configuration](#configuration)
- [Deployment Scenarios](#deployment-scenarios)
- [Docker Compose Services](#docker-compose-services)
- [Volume Management](#volume-management)
- [Networking](#networking)
- [Monitoring](#monitoring)
- [Backup and Restore](#backup-and-restore)
- [Troubleshooting](#troubleshooting)
- [Production Best Practices](#production-best-practices)

---

## Prerequisites

- **Docker** 20.10+ installed
- **Docker Compose** 2.0+ installed
- **Minimum Resources**:
  - 2 CPU cores
  - 2GB RAM
  - 10GB disk space (more for historical data)

**Install Docker**:

```bash
# macOS
brew install --cask docker

# Ubuntu/Debian
curl -fsSL https://get.docker.com -o get-docker.sh
sudo sh get-docker.sh
sudo usermod -aG docker $USER

# Start Docker
sudo systemctl start docker
sudo systemctl enable docker
```

**Verify Installation**:
```bash
docker --version
docker-compose --version
```

---

## Quick Start

### 1. Clone and Configure

```bash
# Navigate to project directory
cd perps-stats

# Copy environment file
cp .env.docker.example .env.docker

# Edit environment variables
nano .env.docker
```

### 2. Create Symbols File

```bash
# Create symbols.txt with trading pairs to track
cat > symbols.txt <<EOF
BTC
ETH
SOL
AVAX
ARB
EOF
```

### 3. Build and Start Services

```bash
# Build images
docker-compose build

# Start all services (detached mode)
docker-compose up -d

# View logs
docker-compose logs -f perps-stats
```

### 4. Initialize Database

```bash
# Run database initialization
docker-compose exec perps-stats perps-stats db init

# Verify database
docker-compose exec postgres psql -U perps -d perps -c "\dt"
```

### 5. Check Status

```bash
# Check running containers
docker-compose ps

# Check database statistics
docker-compose exec perps-stats perps-stats db stats
```

---

## Configuration

### Environment Variables

Edit `.env.docker` to customize:

```bash
# PostgreSQL
POSTGRES_PASSWORD=your_secure_password
POSTGRES_PORT=5432

# Logging
RUST_LOG=perps_stats=info,perps_core=debug

# Grafana (if enabled)
GRAFANA_USER=admin
GRAFANA_PASSWORD=secure_password
GRAFANA_PORT=3000
```

### Application Configuration

Modify the `command` section in `docker-compose.yml`:

```yaml
services:
  perps-stats:
    command:
      - perps-stats
      - start
      - --exchanges
      - binance,hyperliquid,pacifica
      - --klines-interval
      - "60"
      - --report-interval
      - "30"
      - --klines-timeframes
      - "5m,15m,1h,4h,1d"
```

---

## Deployment Scenarios

### Scenario 1: Continuous Data Collection (Default)

Collects ticker, liquidity, slippage, and klines data continuously.

```yaml
command:
  - perps-stats
  - start
  - --klines-interval
  - "60"
  - --report-interval
  - "30"
  - --klines-timeframes
  - "5m,15m,1h"
```

**Start**:
```bash
docker-compose up -d
```

---

### Scenario 2: Historical Backfill

Backfill historical data before starting continuous collection.

**Step 1: Backfill**

Edit `docker-compose.yml`:
```yaml
command:
  - perps-stats
  - backfill
  - -s
  - BTC,ETH,SOL
  - --intervals
  - 1h,1d
  - --start-date
  - "2024-01-01"
```

```bash
docker-compose up perps-stats
# Wait for backfill to complete
docker-compose down
```

**Step 2: Switch to Continuous Collection**

Change command back to `start` and restart:
```bash
docker-compose up -d
```

---

### Scenario 3: Real-Time Streaming

Stream WebSocket data for specific exchanges.

```yaml
command:
  - perps-stats
  - stream
  - --exchange
  - binance
  - -s
  - BTC,ETH
  - --data-types
  - ticker,trade,orderbook
```

```bash
docker-compose up -d
```

---

### Scenario 4: Periodic Excel Exports

Generate Excel reports periodically.

```yaml
command:
  - perps-stats
  - run
  - --interval
  - "300"
  - --output-dir
  - /data/reports
```

```bash
docker-compose up -d

# Access reports
docker-compose exec perps-stats ls -lh /data/reports
```

---

### Scenario 5: Multi-Container Deployment

Run separate containers for different exchanges or tasks.

**docker-compose.override.yml**:
```yaml
version: '3.8'

services:
  perps-binance:
    extends:
      service: perps-stats
    container_name: perps-binance
    command:
      - perps-stats
      - start
      - --exchanges
      - binance
      - --klines-interval
      - "60"

  perps-hyperliquid:
    extends:
      service: perps-stats
    container_name: perps-hyperliquid
    command:
      - perps-stats
      - start
      - --exchanges
      - hyperliquid
      - --klines-interval
      - "60"

  perps-pacifica:
    extends:
      service: perps-stats
    container_name: perps-pacifica
    command:
      - perps-stats
      - start
      - --exchanges
      - pacifica
      - --klines-interval
      - "60"
```

```bash
docker-compose -f docker-compose.yml -f docker-compose.override.yml up -d
```

---

## Docker Compose Services

### PostgreSQL with TimescaleDB

**Features**:
- TimescaleDB extension for time-series optimization
- Persistent volume for data
- Health checks
- Automatic schema initialization

**Access**:
```bash
# Connect to database
docker-compose exec postgres psql -U perps -d perps

# Run SQL queries
docker-compose exec postgres psql -U perps -d perps -c "SELECT COUNT(*) FROM tickers;"
```

---

### perps-stats Application

**Features**:
- Multi-stage build for optimized image size
- Non-root user for security
- Automatic dependency caching
- Resource limits

**Commands**:
```bash
# Execute CLI commands
docker-compose exec perps-stats perps-stats market -s BTC
docker-compose exec perps-stats perps-stats liquidity -s BTC --exchange binance
docker-compose exec perps-stats perps-stats db stats

# Shell access
docker-compose exec perps-stats /bin/bash
```

---

### Grafana (Optional)

Uncomment in `docker-compose.yml` to enable.

**Access**:
- URL: http://localhost:3000
- Default credentials: admin/admin (change via `.env.docker`)

**Configuration**:
```bash
# Configure PostgreSQL data source
docker-compose exec grafana grafana-cli plugins install grafana-postgresql-datasource
```

See `docs/GRAFANA_INTEGRATION.md` for dashboard setup.

---

## Volume Management

### Persistent Volumes

Docker Compose creates named volumes:

1. **postgres_data**: PostgreSQL database files
2. **data_volume**: Application output files (Excel, CSV)
3. **grafana_data** (optional): Grafana dashboards and settings

### Volume Operations

**List volumes**:
```bash
docker volume ls | grep perps
```

**Inspect volume**:
```bash
docker volume inspect perps-postgres-data
```

**Backup volume**:
```bash
# Backup PostgreSQL data
docker run --rm \
  -v perps-postgres-data:/data \
  -v $(pwd)/backups:/backup \
  alpine tar czf /backup/postgres-backup-$(date +%Y%m%d-%H%M%S).tar.gz -C /data .
```

**Restore volume**:
```bash
# Restore PostgreSQL data
docker run --rm \
  -v perps-postgres-data:/data \
  -v $(pwd)/backups:/backup \
  alpine sh -c "cd /data && tar xzf /backup/postgres-backup-20250114-120000.tar.gz"
```

**Clean volumes**:
```bash
# Stop services
docker-compose down

# Remove volumes (WARNING: deletes all data)
docker-compose down -v
```

---

## Networking

### Network Configuration

Docker Compose creates a bridge network `perps-network` connecting:
- `postgres` (accessible as `postgres:5432` from containers)
- `perps-stats` (no exposed ports by default)
- `grafana` (exposed on host port 3000)

### Port Mapping

**Expose PostgreSQL** (for external access):
```yaml
services:
  postgres:
    ports:
      - "5432:5432"  # Host:Container
```

**Access from host**:
```bash
psql -h localhost -p 5432 -U perps -d perps
```

**Expose Application REST API** (when implemented):
```yaml
services:
  perps-stats:
    ports:
      - "8080:8080"
```

---

## Monitoring

### Log Management

**View logs**:
```bash
# All services
docker-compose logs

# Specific service
docker-compose logs perps-stats
docker-compose logs postgres

# Follow logs (real-time)
docker-compose logs -f --tail=100 perps-stats

# Filter by time
docker-compose logs --since 1h perps-stats
```

**Log rotation** is configured in `docker-compose.yml`:
```yaml
logging:
  driver: "json-file"
  options:
    max-size: "10m"
    max-file: "5"
```

### Health Checks

**PostgreSQL health check**:
```bash
docker-compose exec postgres pg_isready -U perps
```

**Application health check**:
```bash
docker-compose exec perps-stats perps-stats --version
```

### Resource Usage

**Monitor resource consumption**:
```bash
# Real-time stats
docker stats

# Container-specific stats
docker stats perps-stats perps-postgres
```

**Resource limits** (in `docker-compose.yml`):
```yaml
deploy:
  resources:
    limits:
      cpus: '2.0'
      memory: 2G
    reservations:
      cpus: '0.5'
      memory: 512M
```

---

## Backup and Restore

### Database Backup

**Manual backup**:
```bash
# Create backup
docker-compose exec postgres pg_dump -U perps -d perps -F c -f /tmp/perps_backup.dump

# Copy to host
docker cp perps-postgres:/tmp/perps_backup.dump ./backups/perps_backup_$(date +%Y%m%d).dump
```

**Automated backup script**:
```bash
#!/bin/bash
# backup.sh
BACKUP_DIR="./backups"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)

mkdir -p $BACKUP_DIR

docker-compose exec -T postgres pg_dump -U perps -d perps -F c \
  > $BACKUP_DIR/perps_backup_$TIMESTAMP.dump

# Keep only last 7 days
find $BACKUP_DIR -name "perps_backup_*.dump" -mtime +7 -delete

echo "Backup completed: $BACKUP_DIR/perps_backup_$TIMESTAMP.dump"
```

**Schedule with cron**:
```bash
# Add to crontab
0 2 * * * /path/to/perps-stats/backup.sh >> /var/log/perps-backup.log 2>&1
```

### Database Restore

```bash
# Stop application (keep database running)
docker-compose stop perps-stats

# Copy backup to container
docker cp ./backups/perps_backup_20250114.dump perps-postgres:/tmp/

# Restore database
docker-compose exec postgres pg_restore -U perps -d perps -c /tmp/perps_backup_20250114.dump

# Restart application
docker-compose start perps-stats
```

---

## Troubleshooting

### Issue: Container Fails to Start

**Check logs**:
```bash
docker-compose logs perps-stats
```

**Common causes**:
1. Database not ready: Wait for health check to pass
2. Permission issues: Check file ownership
3. Port conflicts: Change ports in `.env.docker`

**Solution**:
```bash
# Restart services
docker-compose restart

# Force recreate
docker-compose up -d --force-recreate
```

---

### Issue: Database Connection Errors

**Verify database is running**:
```bash
docker-compose ps postgres
docker-compose exec postgres pg_isready -U perps
```

**Check connection string**:
```bash
docker-compose exec perps-stats env | grep DATABASE_URL
```

**Test connection**:
```bash
docker-compose exec perps-stats perps-stats db stats
```

---

### Issue: Out of Disk Space

**Check disk usage**:
```bash
# Docker disk usage
docker system df

# Container-specific usage
docker-compose exec postgres du -sh /var/lib/postgresql/data
```

**Clean up**:
```bash
# Remove old logs
docker-compose exec postgres sh -c "find /var/lib/postgresql/data/log -name '*.log' -mtime +7 -delete"

# Prune Docker
docker system prune -a --volumes
```

---

### Issue: Slow Performance

**Check resource limits**:
```bash
docker stats perps-stats perps-postgres
```

**Increase resources** in `docker-compose.yml`:
```yaml
deploy:
  resources:
    limits:
      cpus: '4.0'
      memory: 4G
```

**Optimize database**:
```bash
docker-compose exec postgres psql -U perps -d perps -c "VACUUM ANALYZE;"
```

---

### Issue: Container Keeps Restarting

**Check exit code**:
```bash
docker inspect perps-stats | grep ExitCode
```

**Check logs for errors**:
```bash
docker-compose logs --tail=50 perps-stats
```

**Disable restart policy temporarily**:
```yaml
services:
  perps-stats:
    restart: "no"
```

---

## Production Best Practices

### 1. Security

**Change default passwords**:
```bash
# Generate secure password
openssl rand -base64 32

# Update .env.docker
POSTGRES_PASSWORD=<generated_password>
```

**Use Docker secrets** (Docker Swarm):
```yaml
services:
  postgres:
    environment:
      POSTGRES_PASSWORD_FILE: /run/secrets/postgres_password
    secrets:
      - postgres_password

secrets:
  postgres_password:
    file: ./secrets/postgres_password.txt
```

**Run as non-root user** (already configured in Dockerfile).

---

### 2. High Availability

**Use Docker Swarm or Kubernetes** for multi-node deployment:

```bash
# Initialize swarm
docker swarm init

# Deploy stack
docker stack deploy -c docker-compose.yml perps-stack

# Scale service
docker service scale perps-stack_perps-stats=3
```

---

### 3. Monitoring

**Add Prometheus metrics** (future enhancement):
```yaml
services:
  prometheus:
    image: prom/prometheus
    volumes:
      - ./prometheus.yml:/etc/prometheus/prometheus.yml
      - prometheus_data:/prometheus
    ports:
      - "9090:9090"
```

**Enable Grafana** for visualization (see Grafana section).

---

### 4. Backup Strategy

- **Automated daily backups** with `backup.sh` script
- **Offsite backups** to S3 or cloud storage
- **Retention policy**: Keep 7 daily, 4 weekly, 6 monthly backups
- **Test restores regularly**

---

### 5. Log Aggregation

**Use centralized logging** (e.g., ELK stack, Loki):

```yaml
services:
  perps-stats:
    logging:
      driver: "loki"
      options:
        loki-url: "http://loki:3100/loki/api/v1/push"
```

---

### 6. Resource Planning

**Estimate resource needs**:

| Metric | Small (1-5 symbols) | Medium (10-20 symbols) | Large (50+ symbols) |
|--------|---------------------|------------------------|---------------------|
| CPU | 1 core | 2 cores | 4+ cores |
| RAM | 1GB | 2GB | 4GB+ |
| Storage | 10GB | 50GB | 200GB+ |
| Network | 10 Mbps | 50 Mbps | 100 Mbps+ |

---

### 7. Update Strategy

**Zero-downtime updates**:

```bash
# Build new image
docker-compose build

# Update service (rolling update)
docker-compose up -d --no-deps --build perps-stats

# Verify
docker-compose ps
docker-compose logs -f perps-stats
```

**Rollback**:
```bash
# Tag images before updates
docker tag perps-stats:latest perps-stats:v0.2.0

# Rollback if needed
docker tag perps-stats:v0.2.0 perps-stats:latest
docker-compose up -d --force-recreate perps-stats
```

---

## Next Steps

1. **Enable Grafana**: Uncomment Grafana service in `docker-compose.yml` and configure dashboards (see `docs/GRAFANA_INTEGRATION.md`)
2. **Set up automated backups**: Configure `backup.sh` script with cron
3. **Configure monitoring**: Add Prometheus + Grafana metrics
4. **Optimize database**: Enable TimescaleDB hypertables for better time-series performance
5. **Scale horizontally**: Deploy multiple containers for different exchanges

---

## Additional Resources

- **Docker Documentation**: https://docs.docker.com/
- **Docker Compose Reference**: https://docs.docker.com/compose/compose-file/
- **TimescaleDB Documentation**: https://docs.timescale.com/
- **PostgreSQL Backup Guide**: https://www.postgresql.org/docs/current/backup.html

---

**Last Updated**: 2025-10-14
**Version**: 1.0
**Maintainer**: perps-stats team
