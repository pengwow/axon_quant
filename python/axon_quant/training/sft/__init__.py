"""SFT 训练管线（0.11.0 E11）

trajectory → filter → format → train
"""

from .filter import FilterConfig, filter_trajectory
from .format import format_episodes

__all__ = ["FilterConfig", "filter_trajectory", "format_episodes"]
