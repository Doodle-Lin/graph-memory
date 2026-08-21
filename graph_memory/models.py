"""Graph Memory — 数据模型"""
from datetime import datetime
from typing import Optional
from pydantic import BaseModel, Field


class NodeCreate(BaseModel):
    """创建知识节点"""
    content: str = Field(..., description="知识内容/描述")
    title: Optional[str] = Field(None, description="节点标题(可自动从content截取)")
    node_type: str = Field("knowledge", description="节点类型: knowledge/preference/project/fact/skill/reference")
    source: str = Field("manual", description="来源: hermes/claude/codex/manual")
    metadata: dict = Field(default_factory=dict, description="附加元数据")


class NodeUpdate(BaseModel):
    """更新知识节点"""
    content: Optional[str] = None
    title: Optional[str] = None
    node_type: Optional[str] = None
    metadata: Optional[dict] = None


class EdgeCreate(BaseModel):
    """创建边/关联"""
    source: str = Field(..., description="源节点ID")
    target: str = Field(..., description="目标节点ID")
    relation: str = Field("related_to", description="关系类型: related_to/depends_on/part_of/derived_from/...")
    weight: float = Field(1.0, description="边权重")
    metadata: dict = Field(default_factory=dict)


class MemoryWrite(BaseModel):
    """Agent 写入新知识(同时自动建边)"""
    content: str = Field(..., description="新知识内容")
    title: Optional[str] = None
    node_type: str = Field("knowledge")
    source: str = Field("agent")
    auto_link: bool = Field(True, description="自动找关联节点并建边")
    max_links: int = Field(5, description="自动建边数量上限")


class MemoryQuery(BaseModel):
    """检索记忆"""
    query: str = Field(..., description="检索关键词或问题")
    top_k: int = Field(10)
    spread: bool = Field(True, description="是否沿图扩散(PageRank)")
    spread_depth: float = Field(0.85, description="扩散强度(alpha越小扩散越远)")


class GraphSnapshot(BaseModel):
    """完整图快照(给前端渲染)"""
    nodes: list[dict]
    edges: list[dict]
    stats: dict
