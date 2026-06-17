#!/usr/bin/env python3

import re
import uuid
import os
import sys

def update_wxs_file(wxs_path):
    # Read the WIX file
    with open(wxs_path, 'r') as file:
        content = file.read()
    
    # Generate new GUIDs
    upgrade_guid = str(uuid.uuid4())
    component_guid = str(uuid.uuid4())
    uninstall_guid = str(uuid.uuid4())
    
    # Update content with new GUIDs
    content = re.sub(r'Manufacturer="YourName"', f'Manufacturer="TharukRenuja"', content)
    content = re.sub(r'UpgradeCode="PUT-GUID-HERE"', f'UpgradeCode="{upgrade_guid}"', content)
    content = re.sub(r'Guid="PUT-GUID-HERE"', f'Guid="{component_guid}"', content, count=1)
    content = re.sub(r'Guid="PUT-GUID-HERE"', f'Guid="{uninstall_guid}"', content, count=1)
    content = re.sub(r'Key="Software\\\\YourName\\\\rxdl"', 'Key="Software\\TharukRenuja\\rxdl"', content)
    
    # Write the updated content back to the file
    with open(wxs_path, 'w') as file:
        file.write(content)
    
    # Print the GUIDs for environment variables
    print(f'UPGRADE_GUID={upgrade_guid}')
    print(f'COMPONENT_GUID={component_guid}')
    print(f'UNINSTALL_GUID={uninstall_guid}')

if __name__ == '__main__':
    if len(sys.argv) != 2:
        print('Usage: python update_wxs.py <path_to_wxs_file>')
        sys.exit(1)
    
    wxs_path = sys.argv[1]
    if not os.path.exists(wxs_path):
        print(f'Error: File {wxs_path} does not exist.')
        sys.exit(1)
    
    update_wxs_file(wxs_path)