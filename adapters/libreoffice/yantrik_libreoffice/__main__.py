"""`python3 -m yantrik_libreoffice`: the adapter, as `bin/yantrik-libreoffice-adapter` runs it."""

import sys

from .adapter import main

sys.exit(main())
