#!/usr/bin/python

import subprocess
import os

tag_start = "<!-- HELP_START -->"
tag_end = "<!-- HELP_END -->"
readme_file = "README.md"


# Set up a clean environment with only essential variables
# To prevent leaking defaults or sensitive data from a local .env
def load_env_file(file_path):
    env_vars = {}
    if os.path.exists(file_path):
       with open(file_path, 'r') as file:
           for line in file:
               line = line.strip()
               if line and not line.startswith('#'):
                   key, _, _ = line.partition('=')
                   env_vars[key] = ''
    return env_vars
clean_env = load_env_file('.env')

help_text = subprocess.check_output(
      ['cargo', 'run', '--bin', 'blobshare', '--', '--help'],
      env={ **clean_env, 'PATH': os.environ['PATH'] }
).decode('utf-8')

with open(readme_file, 'r') as file:
    data = file.read()

start_parts = data.split(tag_start)
start = start_parts[0]

end = start_parts[1].split(tag_end)[1] 

out = start + tag_start + "\n```\n" + help_text + "\n```\n" + tag_end + end
print(out)

with open(readme_file, 'w') as file:
   file.write(out)


