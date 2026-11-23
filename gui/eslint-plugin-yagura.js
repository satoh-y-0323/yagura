/**
 * The gui's own lint rules. They read types, so they run only where the
 * config gives the parser a program.
 *
 * A brand is a string type made unforgeable by a `unique symbol` key
 * (`DisplayLine` in `src/display/untrusted-text.ts`): only its maker module may
 * claim a value is one. The rules here find a brand by that key's declaration,
 * so a type that merely shares a brand's name is not one.
 */
import ts from 'typescript';

/** How deep a type is walked before it is taken to hold every brand. */
const MAX_DEPTH = 12;

const OPAQUE_FLAGS = ts.TypeFlags.Any | ts.TypeFlags.Unknown;
// Template literal and string mapping types are strings that no brand key can sit on.
const STRING_SHAPED_FLAGS = ts.TypeFlags.TemplateLiteral | ts.TypeFlags.StringMapping;

function slashed(path) {
  return path.replace(/\\/g, '/');
}

function isFile(fileName, relativePath) {
  const path = slashed(fileName);
  return path === relativePath || path.endsWith(`/${relativePath}`);
}

/**
 * What a rule needs to know about brands in one program: which brands a type
 * holds, and the brand types themselves.
 * @param {ts.Program} program
 * @param {{ type: string, key: string, declaredIn: string, maker: string }[]} brands
 */
function createBrandReader(program, brands) {
  const checker = program.getTypeChecker();
  const brandTypes = new Map();

  function brandType(brand) {
    if (!brandTypes.has(brand)) {
      const file = program.getSourceFiles().find((f) => isFile(f.fileName, brand.declaredIn));
      const alias = file?.statements.find(
        (statement) => ts.isTypeAliasDeclaration(statement) && statement.name.text === brand.type
      );
      brandTypes.set(brand, alias === undefined ? null : checker.getTypeAtLocation(alias.name));
    }
    return brandTypes.get(brand);
  }

  function brandOfProperty(property) {
    for (const declaration of property.declarations ?? []) {
      if (
        ts.isPropertySignature(declaration) &&
        ts.isComputedPropertyName(declaration.name) &&
        ts.isIdentifier(declaration.name.expression)
      ) {
        const key = declaration.name.expression.text;
        const fileName = declaration.getSourceFile().fileName;
        const brand = brands.find((b) => b.key === key && isFile(fileName, b.declaredIn));
        if (brand !== undefined) {
          return brand;
        }
      }
    }
    return undefined;
  }

  /** The brand `type` is a value of: a string type that carries a brand's key. */
  function brandOfText(type) {
    if (
      !type.isIntersection() ||
      !type.types.some((member) => (member.flags & ts.TypeFlags.StringLike) !== 0)
    ) {
      return undefined;
    }
    return checker
      .getPropertiesOfType(type)
      .map(brandOfProperty)
      .find((brand) => brand !== undefined);
  }

  // A key declared without a value (`declare const k: unique symbol`) only marks a type:
  // nothing can be stored under it, so what it is typed as is no place for a value.
  function isPhantomKey(property) {
    return (property.declarations ?? []).some((declaration) => {
      if (
        !ts.isPropertySignature(declaration) ||
        !ts.isComputedPropertyName(declaration.name) ||
        !ts.isIdentifier(declaration.name.expression)
      ) {
        return false;
      }
      const key = checker.getSymbolAtLocation(declaration.name.expression)?.valueDeclaration;
      return (
        key !== undefined &&
        ts.isVariableDeclaration(key) &&
        !key.getSourceFile().isDeclarationFile &&
        (ts.getCombinedModifierFlags(key) & ts.ModifierFlags.Ambient) !== 0
      );
    });
  }

  function isLibraryType(type) {
    const declarations = (type.aliasSymbol ?? type.getSymbol())?.declarations ?? [];
    return (
      declarations.length > 0 &&
      declarations.every((declaration) => declaration.getSourceFile().isDeclarationFile)
    );
  }

  function typeArgumentsOf(type) {
    const fromReference =
      (ts.getObjectFlags(type) & ts.ObjectFlags.Reference) !== 0
        ? checker.getTypeArguments(type)
        : [];
    return [...fromReference, ...(type.aliasTypeArguments ?? [])];
  }

  // A generic type is a brand wherever its constraint lets one in.
  function brandsAcceptedBy(type, found) {
    const constraint = checker.getBaseConstraintOfType(type);
    for (const brand of brands) {
      const branded = brandType(brand);
      if (
        constraint === undefined ||
        (constraint.flags & OPAQUE_FLAGS) !== 0 ||
        branded === null ||
        checker.isTypeAssignableTo(branded, constraint)
      ) {
        found.add(brand);
      }
    }
  }

  function visitObject(type, depth, seen, found) {
    const properties = checker.getPropertiesOfType(type);
    for (const property of properties) {
      const brand = brandOfProperty(property);
      if (brand !== undefined) {
        found.add(brand);
      }
    }
    // A library type is walked by what it was instantiated with, not its members.
    if (isLibraryType(type) || checker.isArrayType(type) || checker.isTupleType(type)) {
      for (const argument of typeArgumentsOf(type)) {
        visit(argument, depth + 1, seen, found);
      }
      return;
    }
    for (const property of properties) {
      if (!isPhantomKey(property)) {
        visit(checker.getTypeOfSymbol(property), depth + 1, seen, found);
      }
    }
    for (const info of checker.getIndexInfosOfType(type)) {
      visit(info.type, depth + 1, seen, found);
    }
    for (const signature of [...type.getCallSignatures(), ...type.getConstructSignatures()]) {
      visit(signature.getReturnType(), depth + 1, seen, found);
    }
    for (const argument of typeArgumentsOf(type)) {
      visit(argument, depth + 1, seen, found);
    }
  }

  function visit(type, depth, seen, found) {
    if (depth > MAX_DEPTH) {
      brands.forEach((brand) => found.add(brand));
      return;
    }
    if (seen.has(type) || (type.flags & (OPAQUE_FLAGS | STRING_SHAPED_FLAGS)) !== 0) {
      return;
    }
    seen.add(type);
    if ((type.flags & ts.TypeFlags.Never) !== 0) {
      // `never` flows into any brand's place.
      brands.forEach((brand) => found.add(brand));
    } else if (type.isUnionOrIntersection()) {
      type.types.forEach((member) => visit(member, depth + 1, seen, found));
    } else if ((type.flags & ts.TypeFlags.Instantiable) !== 0) {
      brandsAcceptedBy(type, found);
    } else if ((type.flags & ts.TypeFlags.Object) !== 0) {
      visitObject(type, depth, seen, found);
    }
  }

  /** The brands `type` holds anywhere a value of a brand could sit. */
  function brandsIn(type) {
    const found = new Set();
    visit(type, 0, new Set(), found);
    return found;
  }

  /**
   * True if `parameter` sits where a caller hands a value in: somewhere in an
   * argument's type that is not on the receiving side of a function the
   * caller passes. Conditional and mapped types are not looked into.
   */
  function handedInBy(type, parameter, positive, depth, seen) {
    if (type === parameter) {
      return positive;
    }
    const key = `${positive}`;
    const seenAs = seen.get(type) ?? new Set();
    if (depth > MAX_DEPTH || seenAs.has(key)) {
      return false;
    }
    seenAs.add(key);
    seen.set(type, seenAs);
    const next = (inner, sign) => handedInBy(inner, parameter, sign, depth + 1, seen);
    if (type.isUnionOrIntersection()) {
      return type.types.some((member) => next(member, positive));
    }
    if ((type.flags & ts.TypeFlags.Object) === 0) {
      return false;
    }
    if (checker.isArrayType(type) || checker.isTupleType(type)) {
      return checker.getTypeArguments(type).some((argument) => next(argument, positive));
    }
    const signatures = [...type.getCallSignatures(), ...type.getConstructSignatures()];
    return (
      checker
        .getPropertiesOfType(type)
        .some((property) => next(checker.getTypeOfSymbol(property), positive)) ||
      checker.getIndexInfosOfType(type).some((info) => next(info.type, positive)) ||
      signatures.some(
        (signature) =>
          next(signature.getReturnType(), positive) ||
          signature.getParameters().some((p) => next(checker.getTypeOfSymbol(p), !positive))
      )
    );
  }

  return { checker, brandsIn, brandOfText, handedInBy, isLibraryType };
}

const BRAND_OPTIONS_SCHEMA = {
  type: 'object',
  properties: {
    brands: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          type: { type: 'string' },
          key: { type: 'string' },
          declaredIn: { type: 'string' },
          maker: { type: 'string' },
        },
        required: ['type', 'key', 'declaredIn', 'maker'],
        additionalProperties: false,
      },
    },
  },
  required: ['brands'],
  additionalProperties: false,
};

function brandNames(brands) {
  return [...brands].map((brand) => brand.type).join(', ');
}

const noBrandForging = {
  meta: {
    type: 'problem',
    docs: {
      description: 'Only the maker module of a brand may claim that a value is of it.',
    },
    schema: [BRAND_OPTIONS_SCHEMA],
    messages: {
      noTypes: 'yagura/no-brand-forging needs type information for this file.',
      assertion:
        'This assertion claims {{brands}} for a value not of it; call the maker ({{makers}}) instead.',
      predicate:
        "A type predicate naming {{brands}} lets unchecked text pass for it; return the maker's value ({{makers}}) instead.",
      declaration:
        'A declaration without a value naming {{brands}} claims a value no maker made ({{makers}}).',
      typeArgument:
        "The type argument {{brands}} names a value the callee makes, not one handed in, so nothing checks it; leave it to inference or pass a maker's value ({{makers}}).",
    },
  },
  create(context) {
    const [{ brands }] = context.options;
    const services = context.sourceCode.parserServices;
    if (services?.program == null || services.esTreeNodeToTSNodeMap === undefined) {
      return {
        Program(node) {
          context.report({ node, messageId: 'noTypes' });
        },
      };
    }
    const reader = createBrandReader(services.program, brands);
    const { checker } = reader;
    const foreign = brands.filter((brand) => !isFile(context.filename, brand.maker));
    const tsNode = (node) => services.esTreeNodeToTSNodeMap.get(node);

    function foreignBrandsIn(type) {
      return [...reader.brandsIn(type)].filter((brand) => foreign.includes(brand));
    }

    function report(node, messageId, held) {
      context.report({
        node,
        messageId,
        data: {
          brands: brandNames(held),
          makers: [...new Set(held.map((b) => b.maker))].join(', '),
        },
      });
    }

    function checkAssertion(node) {
      const assertion = tsNode(node);
      if (ts.isConstTypeReference(assertion.type)) {
        return;
      }
      const target = checker.getTypeFromTypeNode(assertion.type);
      const source = checker.getTypeAtLocation(assertion.expression);
      const claimed = claimedBrands(checker.getNonNullableType(source), target, 0);
      if (claimed.size > 0) {
        report(node, 'assertion', [...claimed]);
      }
    }

    /** An object type whose members can be set beside another's, one by one. */
    function isPlainObject(type) {
      return (
        (type.flags & ts.TypeFlags.Object) !== 0 &&
        !reader.isLibraryType(type) &&
        !checker.isArrayType(type) &&
        !checker.isTupleType(type)
      );
    }

    /**
     * The foreign brands an assertion of `source` to `target` claims. Where the
     * two are plain objects they are set beside each other member by member and
     * through each signature's result, so a place where the source already holds
     * the brand claims nothing; any other shape is judged as a whole.
     */
    function claimedBrands(source, target, depth) {
      const held = foreignBrandsIn(target);
      const isAny = (source.flags & ts.TypeFlags.Any) !== 0;
      if (held.length === 0 || (!isAny && checker.isTypeAssignableTo(source, target))) {
        return new Set();
      }
      const whole = new Set(held);
      if (isAny || depth > MAX_DEPTH || !isPlainObject(source) || !isPlainObject(target)) {
        return whole;
      }
      if (
        checker.getIndexInfosOfType(target).length > 0 ||
        target.getConstructSignatures().length > 0
      ) {
        return whole;
      }
      const targetCalls = target.getCallSignatures();
      const sourceCalls = source.getCallSignatures();
      if (targetCalls.length > 1 || targetCalls.length !== sourceCalls.length) {
        return whole;
      }
      const claimed = new Set();
      const add = (inner) => inner.forEach((brand) => claimed.add(brand));
      for (const property of checker.getPropertiesOfType(target)) {
        const counterpart = checker.getPropertyOfType(source, property.name);
        if (counterpart === undefined) {
          return whole;
        }
        add(
          claimedBrands(
            checker.getNonNullableType(checker.getTypeOfSymbol(counterpart)),
            checker.getTypeOfSymbol(property),
            depth + 1
          )
        );
      }
      const [targetCall] = targetCalls;
      const [sourceCall] = sourceCalls;
      if (targetCall !== undefined && sourceCall !== undefined) {
        add(claimedBrands(sourceCall.getReturnType(), targetCall.getReturnType(), depth + 1));
      }
      return claimed;
    }

    function checkReturnTypes(node) {
      const declaration = tsNode(node);
      const signature = checker.getSignatureFromDeclaration(declaration);
      if (signature === undefined) {
        return;
      }
      const held = foreignBrandsIn(signature.getReturnType());
      if (held.length > 0) {
        report(node, 'declaration', held);
      }
    }

    function checkTypeArguments(node) {
      const call = tsNode(node);
      const explicit = node.typeArguments?.params ?? [];
      const resolved = checker.getResolvedSignature(call);
      const declaration = resolved?.getDeclaration();
      const generic =
        declaration === undefined ? undefined : checker.getSignatureFromDeclaration(declaration);
      const parameters = generic?.getTypeParameters() ?? [];
      explicit.forEach((argumentNode, index) => {
        const argument = checker.getTypeFromTypeNode(tsNode(argumentNode));
        if ((argument.flags & OPAQUE_FLAGS) !== 0) {
          return;
        }
        const held = foreignBrandsIn(argument);
        if (held.length === 0) {
          return;
        }
        const parameter = parameters[index];
        const handedIn =
          parameter !== undefined &&
          generic
            .getParameters()
            .some((p) =>
              reader.handedInBy(checker.getTypeOfSymbol(p), parameter, true, 0, new Map())
            );
        if (!handedIn) {
          report(argumentNode, 'typeArgument', held);
        }
      });
    }

    function isAmbient(node) {
      return node.declare === true || (tsNode(node).flags & ts.NodeFlags.Ambient) !== 0;
    }

    return {
      TSAsExpression: checkAssertion,
      TSTypeAssertion: checkAssertion,
      TSTypePredicate(node) {
        if (node.typeAnnotation == null) {
          return;
        }
        const predicated = tsNode(node.typeAnnotation.typeAnnotation);
        const held = foreignBrandsIn(checker.getTypeFromTypeNode(predicated));
        if (held.length > 0) {
          report(node, 'predicate', held);
        }
      },
      VariableDeclaration(node) {
        if (!isAmbient(node)) {
          return;
        }
        for (const declarator of node.declarations) {
          const held = foreignBrandsIn(checker.getTypeAtLocation(tsNode(declarator.id)));
          if (held.length > 0) {
            report(declarator, 'declaration', held);
          }
        }
      },
      PropertyDefinition(node) {
        if (node.declare === true) {
          const held = foreignBrandsIn(checker.getTypeAtLocation(tsNode(node)));
          if (held.length > 0) {
            report(node, 'declaration', held);
          }
        }
      },
      // A function or method without a body: `declare function`, or an overload's signature.
      TSDeclareFunction: checkReturnTypes,
      TSEmptyBodyFunctionExpression(node) {
        if (node.parent?.type === 'MethodDefinition') {
          checkReturnTypes(node.parent);
        }
      },
      'CallExpression[typeArguments]': checkTypeArguments,
      'NewExpression[typeArguments]': checkTypeArguments,
      'TaggedTemplateExpression[typeArguments]': checkTypeArguments,
    };
  },
};

// What a template may show besides a brand: values that are not text, and nothing at all.
const NOT_TEXT_FLAGS =
  ts.TypeFlags.NumberLike |
  ts.TypeFlags.BigIntLike |
  ts.TypeFlags.BooleanLike |
  ts.TypeFlags.Null |
  ts.TypeFlags.Undefined;

// The attributes of a native element that a person reads or hears.
const VISIBLE_ATTRIBUTES = /^(aria-label|title|placeholder|alt)$/;

const displayText = {
  meta: {
    type: 'problem',
    docs: {
      description:
        'Text a template shows is a brand that says how it was prepared: UiText, DisplayLine or DisplayBlock.',
    },
    schema: [BRAND_OPTIONS_SCHEMA],
    messages: {
      noTypes: 'yagura/display-text needs type information for this file.',
      unprepared:
        'This shows {{type}}, which says nothing of how it was prepared; show the UI text from ui-messages or the prepared text from display/untrusted-text.',
      untyped:
        'This shows a value typed {{type}}, so nothing checks how it was prepared; give it the type of the text it holds.',
    },
  },
  create(context) {
    const [{ brands }] = context.options;
    const services = context.sourceCode.parserServices;
    if (services?.program == null || services.esTreeNodeToTSNodeMap === undefined) {
      return {
        Program(node) {
          context.report({ node, messageId: 'noTypes' });
        },
      };
    }
    const reader = createBrandReader(services.program, brands);
    const { checker } = reader;

    function membersOf(type) {
      const resolved =
        (type.flags & ts.TypeFlags.Instantiable) !== 0
          ? (checker.getBaseConstraintOfType(type) ?? checker.getUnknownType())
          : type;
      return resolved.isUnion() ? resolved.types : [resolved];
    }

    function check(expression) {
      const type = checker.getTypeAtLocation(services.esTreeNodeToTSNodeMap.get(expression));
      const members = membersOf(type);
      if (members.some((member) => (member.flags & OPAQUE_FLAGS) !== 0)) {
        context.report({
          node: expression,
          messageId: 'untyped',
          data: { type: checker.typeToString(type) },
        });
        return;
      }
      const unprepared = members.some(
        (member) =>
          (member.flags & NOT_TEXT_FLAGS) === 0 && reader.brandOfText(member) === undefined
      );
      if (unprepared) {
        context.report({
          node: expression,
          messageId: 'unprepared',
          data: { type: checker.typeToString(type) },
        });
      }
    }

    return {
      'SvelteMustacheTag[kind="text"]:not(SvelteStartTag SvelteMustacheTag)'(node) {
        check(node.expression);
      },
      "SvelteStartTag[parent.kind='html'] > SvelteAttribute > SvelteMustacheTag"(node) {
        if (VISIBLE_ATTRIBUTES.test(node.parent.key.name)) {
          check(node.expression);
        }
      },
      "SvelteStartTag[parent.kind='html'] > SvelteShorthandAttribute"(node) {
        if (VISIBLE_ATTRIBUTES.test(node.key.name)) {
          check(node.value);
        }
      },
    };
  },
};

export default {
  meta: { name: 'eslint-plugin-yagura' },
  rules: {
    'no-brand-forging': noBrandForging,
    'display-text': displayText,
  },
};
