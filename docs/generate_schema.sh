#!/bin/bash
# Generate complete schema.sql from migration files for documentation purposes
# This is for documentation only - actual database management uses sqlx migrations

echo "-- Complete Database Schema for perps-stats"
echo "-- Generated from migration files on $(date)"
echo "-- DO NOT USE THIS FOR DATABASE INITIALIZATION"
echo "-- Use 'cargo run -- db init' instead"
echo ""
echo "-- ============================================"
echo "-- This file is for documentation purposes only"
echo "-- ============================================"
echo ""

# Concatenate all migration files in order
for file in ../migrations/*.sql; do
    echo ""
    echo "-- ============================================"
    echo "-- Migration: $(basename "$file")"
    echo "-- ============================================"
    echo ""
    cat "$file"
    echo ""
done
