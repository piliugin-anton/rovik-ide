// Trusted image-side collector. libclang resolves declarations under the actual
// configured translation unit; no regular-expression type inference is used.
#include <clang-c/Index.h>
#include <algorithm>
#include <iostream>
#include <map>
#include <set>
#include <string>
#include <tuple>
#include <vector>

static std::string string(CXString value) {
    const char *raw = clang_getCString(value);
    std::string result = raw ? raw : "";
    clang_disposeString(value);
    return result;
}
static std::string quoted(const std::string &value) {
    std::string result = "\"";
    const char hex[] = "0123456789abcdef";
    for (unsigned char ch : value) {
        if (ch == '"' || ch == '\\') { result += '\\'; result += ch; }
        else if (ch < 32) { result += "\\u00"; result += hex[ch >> 4]; result += hex[ch & 15]; }
        else result += ch;
    }
    return result + '"';
}
static std::string path(CXCursor cursor, unsigned *offset = nullptr) {
    CXFile file = nullptr;
    unsigned at = 0;
    clang_getExpansionLocation(clang_getCursorLocation(cursor), &file, nullptr, nullptr, &at);
    if (offset) *offset = at;
    return file ? string(clang_getFileName(file)) : "";
}
static bool project(const std::string &file) {
    return file.rfind("/snapshot/", 0) == 0 || file.rfind("/build/", 0) == 0 || file.rfind("/oracle/", 0) == 0;
}
struct Node { std::string id, file; bool external; };
struct Graph {
    bool incomplete = false;
    std::map<std::string, Node> nodes;
    std::set<std::tuple<std::string, std::string, std::string>> edges;
    std::string add(CXCursor cursor) {
        if (clang_Cursor_isNull(cursor)) return "";
        unsigned offset = 0;
        auto file = path(cursor, &offset);
        auto id = string(clang_getCursorUSR(cursor));
        if (id.empty()) {
            if (file.empty()) return "";
            id = file + ":" + std::to_string(offset) + ":" + std::to_string(cursor.kind);
        }
        if (project(file)) id += "@" + file;
        auto found = nodes.find(id);
        if (found == nodes.end() || clang_isCursorDefinition(cursor))
            nodes[id] = {id, file, !project(file)};
        return id;
    }
    void dependency(const std::string &source, CXCursor target, const char *kind) {
        auto id = add(target);
        if (!source.empty() && !id.empty() && source != id) edges.emplace(source, id, kind);
    }
    void type(const std::string &source, CXType value, const char *kind, std::set<std::string> &seen, unsigned depth = 0) {
        if (depth > 32) { incomplete = true; return; }
        if (value.kind == CXType_Invalid) return;
        auto key = std::to_string(value.kind) + ":" + string(clang_getTypeSpelling(value));
        if (!seen.insert(key).second) return;
        dependency(source, clang_getTypeDeclaration(value), kind);
        type(source, clang_getCanonicalType(value), kind, seen, depth + 1);
        type(source, clang_getPointeeType(value), kind, seen, depth + 1);
        type(source, clang_getArrayElementType(value), kind, seen, depth + 1);
        type(source, clang_getResultType(value), kind, seen, depth + 1);
        int count = clang_getNumArgTypes(value);
        for (int i = 0; i < count; ++i) type(source, clang_getArgType(value, i), kind, seen, depth + 1);
        count = clang_Type_getNumTemplateArguments(value);
        for (int i = 0; i < count; ++i) type(source, clang_Type_getTemplateArgumentAsType(value, i), kind, seen, depth + 1);
    }
};
struct Visit { Graph *graph; std::string owner; };
static CXChildVisitResult visit(CXCursor cursor, CXCursor, CXClientData opaque) {
    auto context = *static_cast<Visit *>(opaque);
    auto file = path(cursor);
    if (!file.empty() && !project(file)) return CXChildVisit_Continue;
    if (clang_isDeclaration(cursor.kind) && project(file)) {
        context.owner = context.graph->add(cursor);
        std::set<std::string> seen;
        const bool signature = cursor.kind == CXCursor_FunctionDecl || cursor.kind == CXCursor_CXXMethod
            || cursor.kind == CXCursor_Constructor || cursor.kind == CXCursor_ConversionFunction
            || cursor.kind == CXCursor_ParmDecl || cursor.kind == CXCursor_FieldDecl;
        const bool visible = clang_getCXXAccessSpecifier(cursor) != CX_CXXPrivate
            && clang_getCXXAccessSpecifier(cursor) != CX_CXXProtected;
        context.graph->type(context.owner, clang_getCursorType(cursor), signature && visible ? "public_type" : "implementation_type", seen);
        if (cursor.kind == CXCursor_TypedefDecl || cursor.kind == CXCursor_TypeAliasDecl)
            context.graph->type(context.owner, clang_getTypedefDeclUnderlyingType(cursor), "public_type", seen);
        // Fields and methods contribute to the containing type's public layout.
        auto parent = clang_getCursorSemanticParent(cursor);
        if (cursor.kind == CXCursor_FieldDecl || cursor.kind == CXCursor_CXXMethod || cursor.kind == CXCursor_Constructor) {
            auto owner = context.graph->add(parent);
            context.graph->dependency(owner, cursor, "public_type");
        }
    }
    auto reference = clang_getCursorReferenced(cursor);
    if (!clang_Cursor_isNull(reference) && !clang_equalCursors(reference, cursor))
        context.graph->dependency(context.owner, reference, "resolved_symbol");
    clang_visitChildren(cursor, visit, &context);
    return CXChildVisit_Continue;
}
int main(int argc, char **argv) {
    if (argc < 2) { std::cerr << "expected compiler arguments\n"; return 2; }
    CXIndex index = clang_createIndex(0, 0);
    CXTranslationUnit unit = nullptr;
    auto error = clang_parseTranslationUnit2(index, nullptr, const_cast<const char **>(argv + 1), argc - 1,
        nullptr, 0, CXTranslationUnit_KeepGoing, &unit);
    if (error != CXError_Success || !unit) { std::cerr << "libclang could not parse configured translation unit\n"; clang_disposeIndex(index); return 2; }
    bool failed = false;
    for (unsigned i = 0; i < clang_getNumDiagnostics(unit); ++i) {
        CXDiagnostic diagnostic = clang_getDiagnostic(unit, i);
        if (clang_getDiagnosticSeverity(diagnostic) >= CXDiagnostic_Error) failed = true;
        std::cerr << string(clang_formatDiagnostic(diagnostic, clang_defaultDiagnosticDisplayOptions())) << '\n';
        clang_disposeDiagnostic(diagnostic);
    }
    if (failed) { clang_disposeTranslationUnit(unit); clang_disposeIndex(index); return 2; }
    Graph graph;
    Visit context{&graph, ""};
    clang_visitChildren(clang_getTranslationUnitCursor(unit), visit, &context);
    if (graph.incomplete) { std::cerr << "type recursion exceeds collector capability\n"; clang_disposeTranslationUnit(unit); clang_disposeIndex(index); return 2; }
    std::cout << "{\"clang_version\":" << quoted(string(clang_getClangVersion())) << ",\"nodes\":[";
    bool first = true;
    for (const auto &[id, node] : graph.nodes) {
        if (!first) std::cout << ','; first = false;
        std::cout << "{\"id\":" << quoted(id) << ",\"path\":" << (node.external ? "null" : quoted(node.file))
                  << ",\"kind\":" << quoted(node.external ? "external" : "symbol") << '}';
    }
    std::cout << "],\"edges\":["; first = true;
    for (const auto &[source, target, kind] : graph.edges) {
        if (!first) std::cout << ','; first = false;
        std::cout << "{\"source\":" << quoted(source) << ",\"target\":" << quoted(target) << ",\"kind\":" << quoted(kind) << '}';
    }
    std::cout << "]}\n";
    clang_disposeTranslationUnit(unit); clang_disposeIndex(index);
}
