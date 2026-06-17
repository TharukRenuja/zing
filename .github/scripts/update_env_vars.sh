#!/bin/bash

# Read GUIDs from stdout of the Python script
UPGRADE_GUID=$(python scripts/update_wxs.py wix/rxdl.wxs | grep UPGRADE_GUID | cut -d'=' -f2)
COMPONENT_GUID=$(python scripts/update_wxs.py wix/rxdl.wxs | grep COMPONENT_GUID | cut -d'=' -f2)
UNINSTALL_GUID=$(python scripts/update_wxs.py wix/rxdl.wxs | grep UNINSTALL_GUID | cut -d'=' -f2)

# Export GUIDs to environment variables
echo "UPGRADE_GUID=$UPGRADE_GUID" >> $GITHUB_ENV
echo "COMPONENT_GUID=$COMPONENT_GUID" >> $GITHUB_ENV
echo "UNINSTALL_GUID=$UNINSTALL_GUID" >> $GITHUB_ENV